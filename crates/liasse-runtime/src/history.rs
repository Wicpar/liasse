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
//! CORE scope: a single-instance linear lineage. Point identity is the commit
//! seat (§22.3); classification against a same-instance same-lineage history is
//! by seat order (fast-forward ahead, rollback behind, same-point equal), and a
//! different instance incarnation or lineage is `unrelated`. Compaction, alternate
//! retained lineages, and nested-module composition points remain documented
//! seams the artifact container already supports.

use std::collections::BTreeMap;

use liasse_artifact::{Artifact, ArtifactBuilder, ArtifactError};
use liasse_ident::{HistoryPoint, LineageId, PointId};
use liasse_store::{AddressStep, InstanceStore, RowAddress};
use liasse_value::Value;
use serde_json::Value as J;

use crate::engine::{compile_definition, Engine};
use crate::error::EngineError;
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

/// The logical coordinate a §19.9 merge conflict concerns: the top-level
/// collection, the conflicted row's key value, and — for a field-level conflict —
/// the field name (absent for a whole-row delete-vs-modify or competing insert).
///
/// This is structured rather than a rendered string so a host correction can
/// address it by its canonical D.3 display path (§D.3): the collection name, the
/// key rendered as an escaped key-text segment, and the field. A rendered
/// `RowAddress` diagnostic string cannot be reversed to that escaped path (a key
/// containing `/` is ambiguous), which is exactly the attack §D.3 escaping
/// defends; carrying `(collection, key, field)` lets the surface recover the
/// escaped coordinate from a real [`MergeOutcome`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictCoordinate {
    collection: String,
    key: Value,
    field: Option<String>,
}

impl ConflictCoordinate {
    /// The top-level collection the conflicted row belongs to.
    #[must_use]
    pub fn collection(&self) -> &str {
        &self.collection
    }

    /// The conflicted row's application-visible key value (§5.4): the lone
    /// component for a single-field key, a struct of the named components for a
    /// composite key.
    #[must_use]
    pub fn key(&self) -> &Value {
        &self.key
    }

    /// The conflicted field name, or `None` for a whole-row conflict
    /// (delete-vs-modify, competing insert).
    #[must_use]
    pub fn field(&self) -> Option<&str> {
        self.field.as_deref()
    }
}

impl serde::Serialize for ConflictCoordinate {
    /// Serialize as a diagnostic object `{ collection, key, field? }` with the key
    /// as its canonical wire value (Annex A). This is the form the §19.9
    /// reconciliation-plan diagnostic carries; the key stays a structured wire
    /// value so a consumer can rebuild the D.3 display path from it.
    fn serialize<Sr: serde::Serializer>(&self, serializer: Sr) -> Result<Sr::Ok, Sr::Error> {
        let mut object = serde_json::Map::new();
        object.insert("collection".to_owned(), J::String(self.collection.clone()));
        object.insert("key".to_owned(), self.key.to_wire());
        if let Some(field) = &self.field {
            object.insert("field".to_owned(), J::String(field.clone()));
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

/// Serialize a minimal §19.6 `history/index.json` for a linear lineage: its
/// single range from genesis to the selected point.
fn history_index_bytes(lineage: &LineageId, point: &PointId) -> Vec<u8> {
    let mut selected = serde_json::Map::new();
    selected.insert("lineage".to_owned(), J::String(lineage.as_str().to_owned()));
    selected.insert("point".to_owned(), J::String(point.as_str().to_owned()));

    let mut lineage_body = serde_json::Map::new();
    lineage_body.insert("origin".to_owned(), J::String("genesis".to_owned()));
    lineage_body.insert("head".to_owned(), J::String(point.as_str().to_owned()));
    lineage_body.insert("ranges".to_owned(), J::Object(serde_json::Map::new()));

    let mut lineages = serde_json::Map::new();
    lineages.insert(lineage.as_str().to_owned(), J::Object(lineage_body));

    let mut index = serde_json::Map::new();
    index.insert("format".to_owned(), J::Number(1u64.into()));
    index.insert("selected".to_owned(), J::Object(selected));
    index.insert("lineages".to_owned(), J::Object(lineages));
    serde_json::to_vec(&J::Object(index)).unwrap_or_default()
}

impl<S: InstanceStore> Engine<S> {
    /// Export the current committed boundary as a verified `.liasse` artifact
    /// (§19.5, §19.7): the active definition, the selected state, and a minimal
    /// history index naming the selected `(lineage, point)`.
    pub fn export(&self) -> Result<Vec<u8>, EngineError> {
        let state = StateSection::capture(self.schema(), self.store()).map_err(EngineError::Store)?;
        let definition = self
            .definition_source()
            .ok_or_else(|| EngineError::Internal("instance has no active definition".to_owned()))?;
        let point = PointId::new(self.head().get().to_string());
        let selected = HistoryPoint::new(self.lineage().clone(), point.clone());
        let index = history_index_bytes(self.lineage(), &point);
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
        Self::from_state(store, &definition, &state, generator).map_err(ImportError::Engine)
    }

    /// Classify an incoming artifact against local retained history (§19.8),
    /// verifying it first.
    pub fn classify(&self, artifact: &[u8]) -> Result<ImportRelation, ImportError> {
        let opened = Artifact::open(artifact)?;
        let manifest = opened.manifest();
        if manifest.instance != *self.instance() || manifest.selected.lineage() != self.lineage() {
            return Ok(ImportRelation::Unrelated);
        }
        let incoming = manifest
            .selected
            .point()
            .as_str()
            .parse::<u64>()
            .map_err(|_| ImportError::Corrupt("selected point is not a commit seat".to_owned()))?;
        let local = self.head().get();
        Ok(match incoming.cmp(&local) {
            std::cmp::Ordering::Equal => ImportRelation::SamePoint,
            std::cmp::Ordering::Greater => ImportRelation::FastForward,
            std::cmp::Ordering::Less => ImportRelation::Rollback,
        })
    }

    /// Import an artifact under a movement `policy` (§19.8): classify it, and when
    /// the relation is a permitted fast-forward or rollback, activate the movement
    /// by moving live state to the incoming point.
    pub fn import(&mut self, artifact: &[u8], policy: &[ImportRelation]) -> Result<ImportReport, ImportError> {
        let relation = self.classify(artifact)?;
        let movement = matches!(relation, ImportRelation::FastForward | ImportRelation::Rollback);
        let applied = movement && policy.contains(&relation);
        if applied {
            let opened = Artifact::open(artifact)?;
            let (_definition, state) = decode_sections(&opened)?;
            self.reinstall_state(&state).map_err(ImportError::Engine)?;
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
        let local = StateSection::capture(schema, self.store())
            .map_err(|e| ImportError::Engine(EngineError::Store(e)))?
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
    let state = StateSection::from_bytes(opened.state_section(), &compilation.compiled)
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
    /// The structured D.3-addressable coordinate of a conflicted `address`
    /// (§D.3): the top-level collection name and the row's application-visible key
    /// (§5.4), with the field for a field-level conflict. The key resolves through
    /// the schema so a single-field key is its scalar and a composite key its
    /// component struct — the form the surface renders as an escaped key-text
    /// segment.
    fn coordinate(&self, address: &RowAddress, field: Option<String>) -> ConflictCoordinate {
        // A merged row is a top-level collection row, so its final step names the
        // collection and carries the key (nested-collection merge is a seam). A
        // `RowAddress` is non-empty by construction, so the step is always present.
        let mut collection = String::new();
        let mut key = Value::None;
        if let Some(step) = last_step(address) {
            collection = step.name().as_str().to_owned();
            key = match self.schema.top_collection(&collection) {
                Some(model) => materialize::key_identity(model, step.key()),
                None => step.key().components().next().cloned().unwrap_or(Value::None),
            };
        }
        ConflictCoordinate { collection, key, field }
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
