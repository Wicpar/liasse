//! Erasure as a driver-facing host verb (SPEC.md §21.2).
//!
//! §21.2 makes `erase(row)` an explicit operation that plans the *same live
//! removal* an ordinary deletion would (step 1), scrubs the retained payload to a
//! digest stub, and returns a durable [`Extract`] for possible reinsertion
//! (§21.3). A delete grant does not silently become an erasure grant, so erasure
//! is reached through an explicitly exposed erasure call, not a plain `call`.
//!
//! This lifts that to the surface. [`SurfaceHost::erase`] routes an erasure
//! surface call exactly like a mutation `call` — so the engine commits the live
//! removal and every open subscription is dragged through it (§12.6) — and, from
//! the row(s) the removal took out of the observable surface view, synthesizes and
//! binds the [`Extract`] the driver reads back (the "extract binds" the §21.2 call
//! result carries). Because the removal flows through ordinary admission, the
//! erased row is then unobservable in live views and absent from a fresh export,
//! exactly as §21.2 requires.
//!
//! ## Runtime seam
//!
//! The engine has no `Engine::erase`: the mutation interpreter's `erase(row)`
//! builtin is a no-op (`liasse-runtime/src/interp.rs`, `exec_bare` falls through),
//! and the standalone [`Erasure`]/[`Extract`] machinery is not wired into
//! `Engine::call`. So a surface call whose bound runtime mutation is a real delete
//! (`.coll - @id`) commits the removal here and this verb binds the extract, but a
//! call bound to the literal `erase(.coll[@id])` builtin still commits nothing
//! until the runtime executes that builtin (plans the removal, scrubs the payload,
//! returns the [`Extract`]). Scrubbing retained *history* bytes to a stub (§21.2
//! steps 3–5) likewise lives in the runtime; the surface export captures live
//! state, so in CORE scope an erased row is absent from the export by virtue of
//! the live removal.

use std::collections::{BTreeMap, BTreeSet};

use liasse_expr::{RowId, RowIdPart};
use liasse_runtime::{Erasure, Extract, Occurrence, Value};
use liasse_store::InstanceStore;
use liasse_value::{Struct, Text};

use crate::address::SurfaceAddress;
use crate::outcome::SurfaceOutcome;
use crate::request::SurfaceCall;
use crate::router::Resolved;

use super::{SurfaceError, SurfaceHost};

/// The result of a surface erasure (§21.2): the committed removal outcome plus the
/// durable extract synthesized from the scrubbed payload.
///
/// The extract is present exactly when the routed removal committed and took at
/// least one row out of the observable surface view. A call that changed nothing —
/// an absent key, or the not-yet-executed `erase(row)` builtin (see module docs) —
/// carries the (`Unchanged`) outcome and no extract.
#[derive(Debug, Clone)]
pub struct EraseOutcome {
    outcome: SurfaceOutcome,
    extract: Option<Extract>,
}

impl EraseOutcome {
    /// The underlying surface outcome of the routed removal (§12).
    #[must_use]
    pub fn outcome(&self) -> &SurfaceOutcome {
        &self.outcome
    }

    /// The durable extract the erasure produced (§21.2 step 6), if a row was
    /// scrubbed. `None` when the routed call committed no removal.
    #[must_use]
    pub fn extract(&self) -> Option<&Extract> {
        self.extract.as_ref()
    }

    /// Consume the outcome into its parts (the surface outcome and the extract).
    #[must_use]
    pub fn into_parts(self) -> (SurfaceOutcome, Option<Extract>) {
        (self.outcome, self.extract)
    }
}

impl<S: InstanceStore> SurfaceHost<S> {
    /// Erase the row an erasure surface call targets (§21.2): route the removal
    /// through ordinary admission (committing it and sweeping every subscription),
    /// then synthesize and bind the [`Extract`] from the rows it took out of the
    /// observable surface view.
    ///
    /// The removal is planned and committed by the engine exactly as a mutation
    /// `call` — an erasure call bound to a delete removes the row and its
    /// `$on_delete` effects (§21.1) — so afterwards the erased row is unobservable
    /// in live views and absent from a fresh [`export`](SurfaceHost::export). The
    /// surface's own contribution is capturing the scrubbed payload and returning
    /// it as the extract a §21.3 reinsertion consumes.
    ///
    /// # Errors
    /// [`SurfaceError::NoConnection`] if `id` is not open; a store fault from
    /// admission or the barrier sweep. Every §10/§11/§12 refusal is carried in the
    /// outcome, not an error.
    pub fn erase(&mut self, id: &str, call: &SurfaceCall) -> Result<EraseOutcome, SurfaceError> {
        let before = self.surface_view_snapshot(call.address());
        let outcome = self.call(id, call)?;
        if !matches!(outcome, SurfaceOutcome::Committed { .. }) {
            // No committed removal — nothing was scrubbed, so no extract (§21.2:
            // extraction/stubbing only happen when the live removal is admitted).
            return Ok(EraseOutcome { outcome, extract: None });
        }
        let after = self.surface_view_snapshot(call.address());
        let extract = extract_of(&before, &after);
        Ok(EraseOutcome { outcome, extract })
    }

    /// Snapshot the rows of the surface's own `$view` at the committed head, each
    /// as its stable [`RowId`] identity (Annex B.5) paired with its field map. The
    /// surface a call addresses exposes the observable view its rows are read from;
    /// diffing the before/after snapshots by *identity* isolates exactly the
    /// scrubbed rows without the surface having to know the receiver's key field.
    /// Identity — not the projected field map — is what the diff keys on, so a
    /// non-injective projection (one that hides the key) cannot let a surviving
    /// sibling mask a removed row. An unresolvable or unreadable view yields an
    /// empty snapshot (no extract), which is the correct fail-closed observation.
    fn surface_view_snapshot(
        &self,
        call_address: &SurfaceAddress,
    ) -> Vec<(RowId, BTreeMap<String, Value>)> {
        let Ok(view_address) = SurfaceAddress::parse(&call_address.surface_prefix()) else {
            return Vec::new();
        };
        let view_name = match self.router.resolve(&view_address) {
            Ok(Resolved::PublicView(binding)) | Ok(Resolved::RoleView { binding, .. }) => {
                binding.view().to_owned()
            }
            _ => return Vec::new(),
        };
        let Ok(Some(result)) = self.engine.view_at_head(&view_name) else {
            return Vec::new();
        };
        result
            .rows()
            .iter()
            .map(|row| {
                let fields =
                    row.fields().map(|(name, value)| (name.clone(), value.clone())).collect();
                (row.id().clone(), fields)
            })
            .collect()
    }
}

/// Build the extract for the rows present in `before` but gone from `after` — the
/// rows the removal scrubbed (§21.2 step 2). The diff keys on each row's stable
/// [`RowId`] identity, so a row removed under a non-injective projection is
/// captured even when a surviving sibling projects to the same field map. Each
/// scrubbed row is recorded under its own occurrence identity (its `RowId`, §B.5)
/// and erased, replacing it with a digest stub while its payload lives on only in
/// the returned extract. `None` when the removal took nothing out of the
/// observable view.
fn extract_of(
    before: &[(RowId, BTreeMap<String, Value>)],
    after: &[(RowId, BTreeMap<String, Value>)],
) -> Option<Extract> {
    let surviving: BTreeSet<&RowId> = after.iter().map(|(id, _)| id).collect();
    let mut history = Erasure::new();
    let mut occurrences = Vec::new();
    for (id, fields) in before {
        if surviving.contains(id) {
            continue;
        }
        let payload = struct_payload(fields);
        let occurrence = Occurrence::new(occurrence_id(id));
        history.record(occurrence.clone(), payload);
        occurrences.push(occurrence);
    }
    if occurrences.is_empty() {
        return None;
    }
    // Every occurrence was just recorded with a payload, so the scrub cannot miss
    // a payload; a failure here would be an internal contradiction rather than an
    // observable outcome, so it collapses to "no extract".
    history.erase(&occurrences).ok()
}

/// A stable text occurrence identity (Annex B.5) for a view row: its [`RowId`]
/// rendered as its ordered key/occurrence parts, each tagged by kind and closed
/// by a record separator so distinct paths render distinctly. Distinct rows carry
/// distinct ids even under a projection that hides the key, so keying the extract
/// by this — rather than by the projected field map — keeps each scrubbed row a
/// separate occurrence.
fn occurrence_id(id: &RowId) -> String {
    let mut text = String::new();
    for part in id.parts() {
        match part {
            RowIdPart::Key(key) => {
                text.push('k');
                text.push_str(key);
            }
            RowIdPart::Occurrence(segment) => {
                text.push('o');
                text.push_str(&segment.to_string());
            }
        }
        text.push('\u{1e}');
    }
    text
}

/// The row's fields as a composite `struct` value — the scrubbed leaf payload.
fn struct_payload(fields: &BTreeMap<String, Value>) -> Value {
    Value::Struct(Struct::new(
        fields.iter().map(|(name, value)| (Text::new(name.clone()), value.clone())),
    ))
}
