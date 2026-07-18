//! History, artifacts, and reconciliation (§19).
//!
//! An [`Engine`] exports its selected committed boundary as a verified `.liasse`
//! artifact ([`Engine::export`]), is restored from one into a fresh instance
//! ([`Engine::restore`]), and classifies an incoming artifact against local
//! retained history ([`Engine::classify`]/[`Engine::import`], §19.8). The
//! three-way merge of §19.9 is a pure function of the shared base, local, and
//! incoming logical states ([`Engine::merge`]).
//!
//! Artifact integrity is entirely the artifact layer's: [`Artifact::open`]
//! performs the recursive §19.8 verification (mimetype, manifest shape, every
//! checksum, nested modules), so a tampered artifact is rejected at
//! restore/import time as an [`ImportError::Artifact`] before any movement is
//! classified — this is the parse-don't-validate boundary for a byte stream.
//!
//! Point identity is lineage-aware ([`crate::lineage::HistoryCursor`]): a history
//! point is `(lineage, position)`, decoupled from the volatile store commit seat
//! so it survives a restore (§19.2) and a rollback. [`Engine::classify`] compares
//! the incoming point to the local point by their *lineage relationship* (§19.8) —
//! a continuation of the local lineage is a fast-forward, an ancestor is a
//! rollback, a divergence from a shared ancestor is a merge, the identical point
//! is already synchronized, and a different instance or a lineage the local
//! ancestry does not know is `unrelated`. CORE tracks the active lineage and the
//! ancestors a rollback branches through; retaining a displaced continuation's own
//! head, reversible compaction, and nested-module composition points remain
//! documented seams the artifact container already supports.

use std::collections::BTreeMap;

use liasse_artifact::{Artifact, ArtifactBuilder, ArtifactError};
use liasse_ident::HistoryPoint;
use liasse_store::{AddressStep, InstanceStore, RowAddress};
use liasse_value::Value;
use serde_json::Value as J;

use crate::engine::{compile_definition, Engine};
use crate::error::EngineError;
use crate::lineage::LineageEntry;
use crate::materialize::{self, FieldMap};
use crate::portable::StateSection;
use crate::schema::Schema;

/// How an incoming artifact relates to local retained history (§19.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportRelation {
    /// The incoming point is the local point — already synchronized.
    SamePoint,
    /// The local point precedes the incoming point — fast-forward available.
    FastForward,
    /// The incoming point precedes the local point — rollback available.
    Rollback,
    /// A shared point followed by divergence — a three-way merge is required.
    Merge,
    /// No shared point — an unrelated-import policy governs.
    Unrelated,
}

/// The observable result of an [`Engine::import`] (§19.8 result shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportReport {
    /// The classification of the incoming artifact against local history.
    pub relation: ImportRelation,
    /// Whether a movement activated (its relation was permitted by the policy).
    pub applied: bool,
}

/// A failure of a history operation over an artifact.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The byte stream failed recursive `.liasse` verification (§19.8) — the
    /// tamper/corruption rejection surface.
    #[error("artifact verification failed: {0}")]
    Artifact(#[from] ArtifactError),
    /// The verified artifact's definition or state section could not be rebuilt.
    #[error(transparent)]
    Engine(EngineError),
    /// A verified section held bytes the runtime could not interpret.
    #[error("artifact content is malformed: {0}")]
    Corrupt(String),
}

/// The class of a §19.9 merge conflict at one logical coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// Both sides changed the same field to incompatible values.
    IncompatibleValue,
    /// One side deleted a row the other side modified.
    DeleteVsModify,
    /// Both sides inserted a different row at the same new key.
    CompetingInsert,
}

/// The §D.3 application address a §19.9 merge conflict is reported at, so a host
/// correction can resolve it by its canonical display path (§19.9).
///
/// This is structured rather than a rendered string so the surface can recover the
/// escaped D.3 path from a real [`MergeOutcome`]: a rendered `RowAddress`
/// diagnostic string cannot be reversed to it (a key containing `/` is ambiguous),
/// which is exactly the attack §D.3 escaping defends.
///
/// A conflict lives either in a keyed collection or on a §8.2 root-singleton
/// member. The singleton case never leaks the internal reserved storage name
/// (`$root`) or its placeholder empty key: §D.1 gives a root member no ancestor
/// collection key, so its address is a bare declaration name (`/flag`), and an
/// empty key segment is not even a well-formed D.3 path (§D.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictCoordinate {
    /// A conflict in a keyed collection, addressed by `/collection/key[/field]`
    /// (§D.3): the top-level collection name, the row's application-visible key
    /// (§5.4), and the field for a field-level conflict (absent for a whole-row
    /// delete-vs-modify or competing insert).
    Row {
        /// The top-level collection the conflicted row belongs to.
        collection: String,
        /// The conflicted row's application-visible key value (§5.4): the lone
        /// component for a single-field key, a struct of the named components for
        /// a composite key.
        key: Value,
        /// The conflicted field, or `None` for a whole-row conflict.
        field: Option<String>,
    },
    /// A conflict on §8.2 root-singleton state, addressed by the member's name-only
    /// §D.3 application address relative to the model root (`/flag`). `member` is
    /// `None` only for a whole-singleton-row conflict (a base-less competing insert
    /// of the reserved row), addressed at the bare model root (`/`); the internal
    /// `$root` name and its empty key never appear.
    RootSingleton {
        /// The conflicted root member, or `None` for a whole-singleton-row conflict.
        member: Option<String>,
    },
}

impl serde::Serialize for ConflictCoordinate {
    /// Serialize as the §19.9 reconciliation-plan diagnostic. A [`Self::Row`] is
    /// `{ collection, key, field? }` with the key as its canonical wire value
    /// (Annex A) so a consumer can rebuild the D.3 display path from it; a
    /// [`Self::RootSingleton`] is `{ member }` — the §8.2 root member's name-only
    /// application address, carrying no collection wrapper, key, or reserved name.
    fn serialize<Sr: serde::Serializer>(&self, serializer: Sr) -> Result<Sr::Ok, Sr::Error> {
        let mut object = serde_json::Map::new();
        match self {
            Self::Row { collection, key, field } => {
                object.insert("collection".to_owned(), J::String(collection.clone()));
                object.insert("key".to_owned(), key.to_wire());
                if let Some(field) = field {
                    object.insert("field".to_owned(), J::String(field.clone()));
                }
            }
            Self::RootSingleton { member } => {
                if let Some(member) = member {
                    object.insert("member".to_owned(), J::String(member.clone()));
                }
            }
        }
        J::Object(object).serialize(serializer)
    }
}

/// One reported conflict: the coordinate it concerns and why it conflicts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeConflict {
    /// The structured logical coordinate (collection, row key, optional field).
    pub coordinate: ConflictCoordinate,
    /// Why the coordinate could not be merged automatically.
    pub kind: ConflictKind,
}

/// The result of a §19.9 automatic merge: the accepted combined state and any
/// reported conflicts. A non-empty `conflicts` is the reconciliation plan the
/// host correction function of §19.9 would act on; the merge is not activated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    /// The unambiguously combined rows (only meaningful when `conflicts` is empty).
    pub merged: BTreeMap<RowAddress, FieldMap>,
    /// The conflicts that block an automatic activation.
    pub conflicts: Vec<MergeConflict>,
}

impl MergeOutcome {
    /// Whether the merge produced an unambiguous, activatable result (§19.9).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// Serialize the §19.6 `history/index.json` for the selected point and the
/// retained lineages the cursor knows: each lineage records its origin (genesis
/// or the `{lineage, point}` it branched from, §19.6) and its head point. Ranges
/// stay an empty object — CORE retains points without reversible compaction, so
/// no range partition is emitted (a documented §19.4 seam).
fn history_index_bytes(selected: &HistoryPoint, lineages: &[LineageEntry]) -> Vec<u8> {
    let mut selected_obj = serde_json::Map::new();
    selected_obj.insert("lineage".to_owned(), J::String(selected.lineage().as_str().to_owned()));
    selected_obj.insert("point".to_owned(), J::String(selected.point().as_str().to_owned()));

    let mut lineages_obj = serde_json::Map::new();
    for entry in lineages {
        let mut body = serde_json::Map::new();
        match &entry.origin {
            None => {
                body.insert("origin".to_owned(), J::String("genesis".to_owned()));
            }
            Some((parent, position)) => {
                let mut origin = serde_json::Map::new();
                origin.insert("lineage".to_owned(), J::String(parent.as_str().to_owned()));
                origin.insert("point".to_owned(), J::String(position.to_string()));
                body.insert("origin".to_owned(), J::Object(origin));
            }
        }
        body.insert("head".to_owned(), J::String(entry.head.to_string()));
        body.insert("ranges".to_owned(), J::Object(serde_json::Map::new()));
        lineages_obj.insert(entry.lineage.as_str().to_owned(), J::Object(body));
    }

    let mut index = serde_json::Map::new();
    index.insert("format".to_owned(), J::Number(1u64.into()));
    index.insert("selected".to_owned(), J::Object(selected_obj));
    index.insert("lineages".to_owned(), J::Object(lineages_obj));
    serde_json::to_vec(&J::Object(index)).unwrap_or_default()
}

impl<S: InstanceStore> Engine<S> {
    /// Export the current committed boundary as a verified `.liasse` artifact
    /// (§19.5, §19.7): the active definition, the selected state, and a minimal
    /// history index naming the selected `(lineage, point)`.
    pub fn export(&self) -> Result<Vec<u8>, EngineError> {
        // §19.5/§20.1/§22.1 fail-closed: refuse to export an instance holding nested
        // keyed-collection rows (§5.4) this build cannot carry, rather than emit an
        // artifact with that live data dropped — an [`EngineError::Unsupported`].
        let state = StateSection::capture(self.schema(), self.store()).map_err(EngineError::from)?;
        let definition = self
            .definition_source()
            .ok_or_else(|| EngineError::Internal("instance has no active definition".to_owned()))?;
        // §19.2: the exported point is the engine's stable logical position, not
        // the volatile store commit seat, so a restore reproduces the same
        // `(lineage, point)` and a continuation advances past it.
        let selected = self.cursor().point();
        let index = history_index_bytes(&selected, &self.cursor().lineages());
        ArtifactBuilder::new(
            self.instance().clone(),
            selected,
            definition.into_bytes(),
            state.to_bytes(),
            index,
        )
        .build()
        .map_err(|error| EngineError::Internal(format!("artifact build failed: {error}")))
    }

    /// Restore an activated instance over `store` from a verified artifact
    /// (§19.10). Verification (§19.8) runs first, so a tampered artifact is an
    /// [`ImportError::Artifact`] and nothing is instantiated.
    pub fn restore<G: crate::generator::Generators>(
        store: S,
        artifact: &[u8],
        generator: &mut G,
    ) -> Result<Self, ImportError> {
        let opened = Artifact::open(artifact)?;
        let (definition, state) = decode_sections(&opened)?;
        // §19.2/§19.10: adopt the artifact's selected `(lineage, point)` and its
        // recorded lineage ancestry, so a re-export reproduces the exact point and
        // a later classify is lineage-aware — rather than restarting at genesis.
        let cursor = crate::lineage::HistoryCursor::restored(&opened.manifest().selected, opened.history_index());
        Self::from_state(store, &definition, &state, cursor, generator).map_err(ImportError::Engine)
    }

    /// Classify an incoming artifact against local retained history (§19.8),
    /// verifying it first. A different instance incarnation is `unrelated`;
    /// otherwise the incoming `(lineage, point)` is compared to the local point by
    /// their lineage relationship — a continuation is a fast-forward, an ancestor
    /// a rollback, a divergence from a shared ancestor a merge, and a lineage the
    /// local ancestry does not know shares no point (`unrelated`).
    pub fn classify(&self, artifact: &[u8]) -> Result<ImportRelation, ImportError> {
        let opened = Artifact::open(artifact)?;
        let manifest = opened.manifest();
        if manifest.instance != *self.instance() {
            return Ok(ImportRelation::Unrelated);
        }
        Ok(self.cursor().classify(&manifest.selected))
    }

    /// Import an artifact under a movement `policy` (§19.8): classify it, and when
    /// the relation is a permitted fast-forward or rollback, activate the movement
    /// by moving live state to the incoming point and advancing the logical cursor
    /// to that point.
    pub fn import(&mut self, artifact: &[u8], policy: &[ImportRelation]) -> Result<ImportReport, ImportError> {
        let relation = self.classify(artifact)?;
        let movement = matches!(relation, ImportRelation::FastForward | ImportRelation::Rollback);
        let applied = movement && policy.contains(&relation);
        if applied {
            let opened = Artifact::open(artifact)?;
            // §19.5/§19.8: a movement restores the selected point, which carries the
            // definition active at that point. Adopt that definition together with
            // the captured state so a movement across a migration stays coherent —
            // the point's shape and values are not reinterpreted under the currently
            // active model (§20.2).
            let (definition, state) = decode_sections(&opened)?;
            self.reinstall_point(&definition, &state).map_err(ImportError::Engine)?;
            // §19.8: the selected point moves to the incoming one — a fast-forward
            // continues the active lineage, a rollback selects the earlier point
            // and displaces the current continuation onto a new branch.
            let selected = opened.manifest().selected.clone();
            match relation {
                ImportRelation::FastForward => self.cursor_mut().apply_fast_forward(&selected),
                ImportRelation::Rollback => self.cursor_mut().apply_rollback(&selected),
                _ => {}
            }
        }
        Ok(ImportReport { relation, applied })
    }

    /// The §19.9 automatic three-way merge over verified artifacts: `base` is the
    /// shared history point, local is this engine's current committed state, and
    /// `incoming` is the other side. Returns the unambiguous combined result plus
    /// any conflicts — the reconciliation plan a host correction would resolve.
    pub fn merge(&self, base: &[u8], incoming: &[u8]) -> Result<MergeOutcome, ImportError> {
        let base = decode_sections(&Artifact::open(base)?)?.1;
        let incoming = decode_sections(&Artifact::open(incoming)?)?.1;
        let schema = self.schema();
        let base = base.working(schema).map_err(ImportError::Engine)?;
        // §19.9/§22.1 fail-closed: the local side of a merge is captured through the
        // same portable path, so refuse when it holds nested keyed-collection rows
        // (§5.4) this build cannot carry rather than merge a lossy local snapshot.
        let local = StateSection::capture(schema, self.store())
            .map_err(|error| ImportError::Engine(EngineError::from(error)))?
            .working(schema)
            .map_err(ImportError::Engine)?;
        let incoming = incoming.working(schema).map_err(ImportError::Engine)?;
        Ok(ThreeWayMerge { base, local, incoming, schema }.resolve())
    }
}

/// Decode the definition text and portable state of a verified artifact.
fn decode_sections(opened: &Artifact) -> Result<(String, StateSection), ImportError> {
    let definition = std::str::from_utf8(opened.liasse_json())
        .map_err(|_| ImportError::Corrupt("definition is not UTF-8".to_owned()))?
        .to_owned();
    let compilation =
        compile_definition(&definition, &crate::host::HostSignatures::default()).map_err(ImportError::Engine)?;
    let state = StateSection::from_bytes(opened.state_section(), &compilation.compiled, &compilation.model)
        .map_err(ImportError::Engine)?;
    Ok((definition, state))
}

/// The three logical states a §19.9 merge compares, keyed by row address, plus
/// the schema that resolves a conflicted address to its structured coordinate.
struct ThreeWayMerge<'a> {
    base: BTreeMap<RowAddress, FieldMap>,
    local: BTreeMap<RowAddress, FieldMap>,
    incoming: BTreeMap<RowAddress, FieldMap>,
    schema: Schema<'a>,
}

impl ThreeWayMerge<'_> {
    /// The D.3-addressable coordinate of a conflicted `address` (§D.3). A keyed
    /// collection row resolves to `/collection/key[/field]`: the top-level
    /// collection name and the row's application-visible key (§5.4), with the field
    /// for a field-level conflict. The key resolves through the schema so a
    /// single-field key is its scalar and a composite key its component struct — the
    /// form the surface renders as an escaped key-text segment.
    ///
    /// The §8.2 singleton reserved row is internal storage, not a collection: a
    /// conflict on one of its members is reported at that member's name-only
    /// application address (`/flag`), never the reserved `$root` name or its
    /// placeholder empty key — which §D.3 forbids as an empty path segment and §D.1
    /// gives no ancestor key.
    fn coordinate(&self, address: &RowAddress, field: Option<String>) -> ConflictCoordinate {
        // A merged row is a top-level collection row, so its final step names the
        // collection and carries the key (nested-collection merge is a seam). A
        // `RowAddress` is non-empty by construction, so the step is always present.
        let Some(step) = last_step(address) else {
            return ConflictCoordinate::Row { collection: String::new(), key: Value::None, field };
        };
        let collection = step.name().as_str();
        if collection == crate::singleton::ROOT_NAME {
            return ConflictCoordinate::RootSingleton { member: field };
        }
        let key = match self.schema.top_collection(collection) {
            Some(model) => materialize::key_identity(model, step.key()),
            None => step.key().components().next().cloned().unwrap_or(Value::None),
        };
        ConflictCoordinate::Row { collection: collection.to_owned(), key, field }
    }

    /// Resolve the merge coordinate by coordinate (§19.9): accept a change made on
    /// one side, equal results on both sides, and compatible changes to separate
    /// coordinates; report incompatible field values, delete-versus-modify, and
    /// competing inserts.
    fn resolve(self) -> MergeOutcome {
        let mut merged = BTreeMap::new();
        let mut conflicts = Vec::new();
        let mut addresses: Vec<&RowAddress> = self
            .base
            .keys()
            .chain(self.local.keys())
            .chain(self.incoming.keys())
            .collect();
        addresses.sort();
        addresses.dedup();
        for address in addresses {
            self.resolve_row(address, &mut merged, &mut conflicts);
        }
        MergeOutcome { merged, conflicts }
    }

    fn resolve_row(
        &self,
        address: &RowAddress,
        merged: &mut BTreeMap<RowAddress, FieldMap>,
        conflicts: &mut Vec<MergeConflict>,
    ) {
        let base = self.base.get(address);
        let local = self.local.get(address);
        let incoming = self.incoming.get(address);
        match (base, local, incoming) {
            // Untouched or identically present everywhere.
            (_, Some(l), Some(i)) if l == i => {
                merged.insert(address.clone(), l.clone());
            }
            // A fresh row on both sides with different content: competing insert.
            (None, Some(_), Some(_)) => conflicts.push(MergeConflict {
                coordinate: self.coordinate(address, None),
                kind: ConflictKind::CompetingInsert,
            }),
            // A fresh row on exactly one side: accept it.
            (None, Some(l), None) => {
                merged.insert(address.clone(), l.clone());
            }
            (None, None, Some(i)) => {
                merged.insert(address.clone(), i.clone());
            }
            // Modified on one side, deleted on the other: delete-vs-modify.
            (Some(b), Some(l), None) if l != b => conflicts.push(MergeConflict {
                coordinate: self.coordinate(address, None),
                kind: ConflictKind::DeleteVsModify,
            }),
            (Some(b), None, Some(i)) if i != b => conflicts.push(MergeConflict {
                coordinate: self.coordinate(address, None),
                kind: ConflictKind::DeleteVsModify,
            }),
            // Deleted on one side, unchanged on the other, or deleted on both:
            // the row is deleted (absent from the merged result).
            (Some(_), None, None)
            | (Some(_), Some(_), None)
            | (Some(_), None, Some(_)) => {}
            // Present in base and both sides: merge field by field.
            (Some(b), Some(l), Some(i)) => {
                self.merge_fields(address, b, l, i, merged, conflicts);
            }
            // No side has the row: nothing to do.
            (None, None, None) => {}
        }
    }

    fn merge_fields(
        &self,
        address: &RowAddress,
        base: &FieldMap,
        local: &FieldMap,
        incoming: &FieldMap,
        merged: &mut BTreeMap<RowAddress, FieldMap>,
        conflicts: &mut Vec<MergeConflict>,
    ) {
        let mut row = FieldMap::new();
        let mut names: Vec<&String> = base.keys().chain(local.keys()).chain(incoming.keys()).collect();
        names.sort();
        names.dedup();
        let before = conflicts.len();
        for name in names {
            let b = base.get(name);
            let l = local.get(name);
            let i = incoming.get(name);
            match Self::merge_field(b, l, i) {
                Ok(Some(value)) => {
                    row.insert(name.clone(), value);
                }
                Ok(None) => {}
                Err(()) => conflicts.push(MergeConflict {
                    coordinate: self.coordinate(address, Some(name.clone())),
                    kind: ConflictKind::IncompatibleValue,
                }),
            }
        }
        if conflicts.len() == before {
            merged.insert(address.clone(), row);
        }
    }

    /// Merge one field's base/local/incoming values (§19.9): a change on one side
    /// wins, equal changes agree, and divergent changes conflict.
    fn merge_field(base: Option<&Value>, local: Option<&Value>, incoming: Option<&Value>) -> Result<Option<Value>, ()> {
        if local == incoming {
            return Ok(local.cloned());
        }
        if local == base {
            return Ok(incoming.cloned());
        }
        if incoming == base {
            return Ok(local.cloned());
        }
        Err(())
    }
}

/// The final address step of a row (the collection + key at its own level). A
/// `RowAddress` is non-empty by construction, so this is `Some` for every real
/// address; the `Option` keeps the coordinate builder total without a panic.
fn last_step(address: &RowAddress) -> Option<&AddressStep> {
    address.steps().last()
}
