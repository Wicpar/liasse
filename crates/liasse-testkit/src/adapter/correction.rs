//! Driving the §19.9 host correction and merge activation.
//!
//! Two engine/surface primitives combine here. A `reconcile` computes the §19.9
//! three-way [`MergeOutcome`] but never commits it; a later `apply_correction`
//! resolves the reconciliation plan's conflicts through the surface host's
//! D.3-addressed [`SurfaceHost::apply_correction`] and then commits the corrected
//! composition into a new lineage via [`Engine::activate_merge`].
//!
//! The adapter's contribution is assembling the *corrected composition* the engine
//! activates. The surface correction validates the choice per conflict (that every
//! conflict is addressed by its escaped D.3 display path and none is left over) but
//! reports only the accepted *side*, not the resolved rows. So the adapter rebuilds
//! the full row set: the clean rows the automatic merge already accepted
//! ([`MergeOutcome::merged`]), plus every conflicted row rebuilt from the three
//! per-side logical states, taking the host's chosen side at each conflicted field
//! and the ordinary §19.9 field merge elsewhere.
//!
//! The three per-side states are read back through the runtime's own machinery, not
//! reconstructed by hand: a side's full row set is the clean self-merge of that
//! side against itself (`engine.merge(bytes, bytes)`, where base == local ==
//! incoming yields exactly that state), so the adapter never re-derives storage
//! addressing or field decoding.

use std::collections::BTreeMap;

use liasse_ident::InstanceId;
use liasse_runtime::{ConflictCoordinate, Engine, MergeConflict, MergeOutcome, Precision, Value};
use liasse_store::{InstanceStore, MemoryStore, RowAddress};
use liasse_surface::{
    ChooseMap, ChooseSide, ConflictCoordinate as SurfaceConflict, VirtualClock as SurfaceClock,
};
use serde_json::json;

use crate::contract::Observation;
use crate::outcome::{Completion, Outcome};

use super::runtime::Runtime;
use super::{AdapterError, EPOCH_MICROS};

/// One collection level's field map — the shape a stored row and a
/// [`MergeOutcome::merged`] entry share.
type Fields = BTreeMap<String, Value>;

/// A logical state keyed by row address: exactly the shape
/// [`Engine::activate_merge`] installs.
type RowState = BTreeMap<RowAddress, Fields>;

/// A retained §19.9 reconciliation plan: the base and incoming artifact bytes a
/// `bind_plan` reconcile computed its merge over. A later `apply_correction`
/// recomputes the same plan against current committed state — no mutation runs
/// between the two steps, so local state is unchanged and the plan is stable.
pub(super) struct ReconcilePlan {
    /// The shared merge base (the ancestor the incoming diverged from).
    pub(super) base: Vec<u8>,
    /// The incoming side's exported `.liasse` bytes.
    pub(super) incoming: Vec<u8>,
}

/// How each conflict on one row is resolved: the chosen side per conflicted field,
/// and — for a whole-row conflict — the side that keeps (or drops) the row.
#[derive(Default)]
struct RowResolution {
    fields: BTreeMap<String, ChooseSide>,
    whole_row: Option<ChooseSide>,
}

impl<S: InstanceStore> Runtime<S> {
    /// §19.9 `apply_correction`: recompute the reconciliation plan over `base`/
    /// `incoming` against current committed state, validate the host correction
    /// (every conflict chosen by its escaped D.3 path, no stray path), then activate
    /// the corrected composition into a new lineage.
    pub(super) fn drive_correction(
        &mut self,
        base: &[u8],
        incoming: &[u8],
        choose: &serde_json::Value,
    ) -> Result<Observation, AdapterError> {
        let choose_map = parse_choose(choose)?;
        let outcome = {
            let loaded = self.loaded()?;
            loaded.host.reconcile(base, incoming).map_err(|error| AdapterError::Host(error.to_string()))?
        };
        // §19.9: the surface correction addresses each conflict by its escaped D.3
        // display path and refuses a stray or incomplete choose. A refused
        // correction is a rejected observation, not an activation.
        let coordinates = surface_coordinates(&outcome.conflicts)?;
        let complete = {
            let loaded = self.loaded()?;
            matches!(
                loaded.host.apply_correction(&coordinates, &choose_map),
                Ok(correction) if correction.is_complete()
            )
        };
        if !complete {
            return Ok(Observation::outcome(Outcome::Rejected));
        }
        let corrected = self.corrected_state(&outcome, base, incoming, &choose_map)?;
        self.rebuild_engine(move |engine| engine.activate_merge(&corrected))?
            .map_err(|error| AdapterError::Host(error.to_string()))?;
        Ok(Observation {
            outcome: Outcome::Ok,
            value: Some(json!({ "applied": true })),
            completion: Some(Completion::Committed),
            extra: serde_json::Map::new(),
        })
    }

    /// The composition §19.9 activates: the clean merged rows plus every conflicted
    /// row rebuilt from the three per-side states under the host's chosen sides.
    fn corrected_state(
        &mut self,
        outcome: &MergeOutcome,
        base: &[u8],
        incoming: &[u8],
        choose: &ChooseMap,
    ) -> Result<RowState, AdapterError> {
        let local = self.local_state()?;
        let base_state = full_state_from_bytes(base)?;
        let incoming_state = full_state_from_bytes(incoming)?;
        // Group each conflict's chosen side by the row it addresses. A row's address
        // is recovered by value from the per-side states (its top-level collection
        // name and single-component key), so no schema-owned key derivation is
        // needed in the adapter.
        let mut rows: BTreeMap<RowAddress, RowResolution> = BTreeMap::new();
        for conflict in &outcome.conflicts {
            let coordinate = &conflict.coordinate;
            let address = find_row(
                &[&local, &incoming_state, &base_state],
                coordinate.collection(),
                coordinate.key(),
            )
            .ok_or_else(|| {
                AdapterError::Host(format!(
                    "correction conflict in `{}` addresses no known row",
                    coordinate.collection()
                ))
            })?;
            let path = surface_coordinate(coordinate)?
                .display_path()
                .map_err(|error| AdapterError::Host(error.to_string()))?;
            let side = choose.get(&path).ok_or_else(|| {
                AdapterError::Host(format!("correction leaves `{path}` unresolved"))
            })?;
            let entry = rows.entry(address).or_default();
            match coordinate.field() {
                Some(field) => {
                    entry.fields.insert(field.to_owned(), side);
                }
                None => entry.whole_row = Some(side),
            }
        }
        let mut corrected = outcome.merged.clone();
        for (address, resolution) in rows {
            let resolved = resolve_row(
                &resolution,
                base_state.get(&address),
                local.get(&address),
                incoming_state.get(&address),
            );
            match resolved {
                Some(row) => {
                    corrected.insert(address, row);
                }
                None => {
                    corrected.remove(&address);
                }
            }
        }
        Ok(corrected)
    }

    /// The active engine's current committed state as a row-addressed map: the clean
    /// self-merge of its export against itself (base == local == incoming yields
    /// exactly that state), reusing the runtime's decode/address machinery.
    fn local_state(&mut self) -> Result<RowState, AdapterError> {
        let loaded = self.loaded()?;
        let bytes = loaded.host.export().map_err(|error| AdapterError::Host(error.to_string()))?;
        let outcome = loaded
            .host
            .engine()
            .merge(&bytes, &bytes)
            .map_err(|error| AdapterError::Host(error.to_string()))?;
        Ok(outcome.merged)
    }
}

/// Parse a `choose` map (display path → side). `"local"`/`"incoming"` map to the
/// two [`ChooseSide`]s; a `{ value }` correction is a documented surface seam
/// (the host correction API carries no provide-a-value variant), reported as a
/// precise skip.
fn parse_choose(choose: &serde_json::Value) -> Result<ChooseMap, AdapterError> {
    let object = choose.as_object().ok_or_else(|| {
        AdapterError::unsupported("`apply_correction` choose must be a display-path -> side object")
    })?;
    let mut map = ChooseMap::new();
    for (path, side) in object {
        let side = match side.as_str() {
            Some("incoming") => ChooseSide::Incoming,
            Some("local") => ChooseSide::Local,
            _ => {
                return Err(AdapterError::unsupported(format!(
                    "`apply_correction` choose `{path}` must be \"incoming\" or \"local\" \
                     (a `{{ value }}` correction is a documented surface-host seam)"
                )));
            }
        };
        map = map.with(path.clone(), side);
    }
    Ok(map)
}

/// The surface conflict coordinates for a plan's conflicts, so the host correction
/// can address each by its escaped D.3 display path.
fn surface_coordinates(conflicts: &[MergeConflict]) -> Result<Vec<SurfaceConflict>, AdapterError> {
    conflicts.iter().map(|conflict| surface_coordinate(&conflict.coordinate)).collect()
}

/// The surface [`SurfaceConflict`] for one runtime conflict coordinate.
fn surface_coordinate(coordinate: &ConflictCoordinate) -> Result<SurfaceConflict, AdapterError> {
    Ok(match coordinate.field() {
        Some(field) => SurfaceConflict::field(coordinate.collection(), coordinate.key().clone(), field),
        None => SurfaceConflict::row(coordinate.collection(), coordinate.key().clone()),
    })
}

/// The full logical state of an exported side: restore it into a throwaway engine,
/// then take its clean self-merge (base == local == incoming), which the runtime
/// materializes as exactly that state, row-addressed and field-decoded.
fn full_state_from_bytes(bytes: &[u8]) -> Result<RowState, AdapterError> {
    let store = MemoryStore::new(InstanceId::new("correction-side"));
    let mut clock = SurfaceClock::new(EPOCH_MICROS, Precision::Micros);
    let engine = Engine::restore(store, bytes, &mut clock)
        .map_err(|error| AdapterError::Host(format!("correction side restore: {error}")))?;
    let outcome = engine
        .merge(bytes, bytes)
        .map_err(|error| AdapterError::Host(format!("correction side merge: {error}")))?;
    Ok(outcome.merged)
}

/// The row address of the top-level `collection` row keyed `key`, recovered by
/// value from the per-side states. CORE scope: a top-level single-component key,
/// which is what a §19.9 merge coordinate carries.
fn find_row(states: &[&RowState], collection: &str, key: &Value) -> Option<RowAddress> {
    for state in states {
        for address in state.keys() {
            let Some(step) = address.steps().last() else { continue };
            if step.name().as_str() != collection {
                continue;
            }
            let mut components = step.key().components();
            if let (Some(first), None) = (components.next(), components.next())
                && first == key
            {
                return Some(address.clone());
            }
        }
    }
    None
}

/// Rebuild one conflicted row under its resolution: a whole-row conflict takes the
/// chosen side's row (or drops it when that side has none); a field conflict takes
/// the chosen side at each conflicted field and the ordinary §19.9 field merge
/// (a change on one side wins, equal changes agree) elsewhere.
fn resolve_row(
    resolution: &RowResolution,
    base: Option<&Fields>,
    local: Option<&Fields>,
    incoming: Option<&Fields>,
) -> Option<Fields> {
    if let Some(side) = resolution.whole_row {
        return side_row(side, local, incoming).cloned();
    }
    let mut names: Vec<&String> =
        [base, local, incoming].into_iter().flatten().flat_map(BTreeMap::keys).collect();
    names.sort();
    names.dedup();
    let mut row = Fields::new();
    for name in names {
        let value = match resolution.fields.get(name) {
            Some(side) => side_row(*side, local, incoming).and_then(|fields| fields.get(name)).cloned(),
            None => merge_field(field(base, name), field(local, name), field(incoming, name)),
        };
        if let Some(value) = value {
            row.insert(name.clone(), value);
        }
    }
    Some(row)
}

/// The chosen side's row.
fn side_row<'a>(side: ChooseSide, local: Option<&'a Fields>, incoming: Option<&'a Fields>) -> Option<&'a Fields> {
    match side {
        ChooseSide::Incoming => incoming,
        ChooseSide::Local => local,
    }
}

/// One field of a row, if present.
fn field<'a>(fields: Option<&'a Fields>, name: &str) -> Option<&'a Value> {
    fields.and_then(|fields| fields.get(name))
}

/// The ordinary §19.9 field merge for a non-conflicted field of a conflicted row: a
/// change on one side wins, equal changes agree. A field the plan reported as a
/// conflict is never merged here (it is resolved by the chosen side), so the final
/// fallthrough is unreachable for a complete plan; it favours the incoming side
/// rather than fabricate a value.
fn merge_field(base: Option<&Value>, local: Option<&Value>, incoming: Option<&Value>) -> Option<Value> {
    if local == incoming {
        return local.cloned();
    }
    if local == base {
        return incoming.cloned();
    }
    if incoming == base {
        return local.cloned();
    }
    incoming.cloned()
}
