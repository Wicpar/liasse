//! Deletion and erasure dynamic semantics (§21): the cascade plan every inbound
//! reference policy induces, and erasure that scrubs retained payload bytes
//! while preserving verifiable history structure.
//!
//! Two operations are kept apart (§21):
//!
//! - **Deletion** removes rows from live state under each inbound ref's
//!   `$on_delete` policy. [`DeletionPlan`] expands every direct and cascading
//!   target to a fixed point (cascade cycles remove each row once), blocks on a
//!   `restrict` ref whose referencing row survives, clears `none` refs and
//!   applies `= patch`es to surviving rows (combining disjoint or equal
//!   assignments, rejecting a conflict), and ignores patches to rows that are
//!   themselves deleted. The plan is computed from the pre-delete state and then
//!   applied atomically (§21.1).
//! - **Erasure** additionally scrubs the retained payload of the erased
//!   occurrence, replacing it with a digest stub that preserves the leaf hash so
//!   history and artifact checksums still verify. Erased bytes are then
//!   unobservable in live views, in history, and on replay; only
//!   [`Erasure::reinsert`] with a matching stub and a hash-clean extract can
//!   restore them (§21.2/§21.3).

use std::collections::{BTreeMap, BTreeSet};

use liasse_host::BlobIntegrity;
use liasse_value::Value;

/// The identity of one row: its collection and key value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RowRef {
    /// The collection name.
    pub collection: String,
    /// The row key.
    pub key: Value,
}

impl RowRef {
    /// Build a row identity.
    #[must_use]
    pub fn new(collection: impl Into<String>, key: Value) -> Self {
        Self { collection: collection.into(), key }
    }
}

/// A resolved `$on_delete` policy on an inbound reference (§21.1, §5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletePolicy {
    /// Reject deletion while the referencing row survives.
    Restrict,
    /// Delete the referencing row.
    Cascade,
    /// Clear this optional ref (assign `none`).
    Clear,
    /// Patch the referencing row with the given planning-time assignments.
    Patch(Vec<(String, Value)>),
    /// Remove this member value from the referencing `$set` field (§5.6/§21.1:
    /// for a set of refs, `cascade` deletes the containing **set member**, not the
    /// whole row). A surviving-row effect — the referencing row is kept and only
    /// its membership of the deleted target is dropped.
    DropMember(Value),
    /// The inbound ref left `$on_delete` UNDECIDED. The §21.1 static gate proves
    /// this can never reach a live deletion for a statically-known ref, so an
    /// undecided edge is a fail-closed backstop only: if the target is removed
    /// while the referencing row survives, the plan is rejected rather than
    /// silently committing a dangling reference (§22.1).
    Undecided,
}

/// One inbound reference edge in the deletion graph: the referencing row, its
/// ref field, the target it points at, and the policy that governs its removal.
#[derive(Debug, Clone)]
pub struct RefEdge {
    /// The referencing row.
    pub from: RowRef,
    /// The referencing field.
    pub field: String,
    /// The target row.
    pub to: RowRef,
    /// The `$on_delete` policy.
    pub policy: DeletePolicy,
}

/// The pre-delete relation graph a plan is computed over (§21.1).
#[derive(Debug, Clone, Default)]
pub struct Graph {
    rows: BTreeMap<RowRef, BTreeMap<String, Value>>,
    edges: Vec<RefEdge>,
}

impl Graph {
    /// A fresh empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a row and its fields.
    pub fn add_row(&mut self, row: RowRef, fields: BTreeMap<String, Value>) {
        self.rows.insert(row, fields);
    }

    /// Add an inbound reference edge.
    pub fn add_edge(&mut self, edge: RefEdge) {
        self.edges.push(edge);
    }

    /// Whether a live row occupies `row`.
    #[must_use]
    pub fn contains(&self, row: &RowRef) -> bool {
        self.rows.contains_key(row)
    }

    /// The fields of a live row, if present.
    #[must_use]
    pub fn fields(&self, row: &RowRef) -> Option<&BTreeMap<String, Value>> {
        self.rows.get(row)
    }

    /// Plan the deletion of `initial` under every inbound policy (§21.1). The
    /// plan is computed from this pre-delete state; applying it is a separate
    /// atomic step.
    pub fn plan(&self, initial: &[RowRef]) -> Result<DeletionPlan, DeleteError> {
        let deletes = self.cascade_closure(initial);
        self.check_restrict(&deletes)?;
        self.check_undecided(&deletes)?;
        let patches = self.collect_patches(&deletes)?;
        let member_removals = self.collect_member_removals(&deletes);
        Ok(DeletionPlan { deletes, patches, member_removals })
    }

    /// Expand the initial targets to the cascade fixed point (§21.1). Each row is
    /// added once, so cascade cycles terminate.
    fn cascade_closure(&self, initial: &[RowRef]) -> BTreeSet<RowRef> {
        let mut deletes: BTreeSet<RowRef> = initial.iter().cloned().collect();
        let mut frontier: Vec<RowRef> = initial.to_vec();
        while let Some(target) = frontier.pop() {
            for edge in &self.edges {
                if edge.to == target
                    && edge.policy == DeletePolicy::Cascade
                    && deletes.insert(edge.from.clone())
                {
                    frontier.push(edge.from.clone());
                }
            }
        }
        deletes
    }

    /// §21.1: a `restrict` ref blocks deletion only when its referencing row is
    /// outside the final delete set.
    fn check_restrict(&self, deletes: &BTreeSet<RowRef>) -> Result<(), DeleteError> {
        for edge in &self.edges {
            if edge.policy == DeletePolicy::Restrict
                && deletes.contains(&edge.to)
                && !deletes.contains(&edge.from)
            {
                return Err(DeleteError::Restricted {
                    referencing: Box::new(edge.from.clone()),
                    field: edge.field.clone(),
                    target: Box::new(edge.to.clone()),
                });
            }
        }
        Ok(())
    }

    /// §22.1/§21.1 fail-closed backstop: an inbound ref that left `$on_delete`
    /// UNDECIDED must never outlive its target's removal — that would commit a
    /// dangling reference. The static §21.1 gate proves this unreachable for a
    /// statically-known ref, so reaching here means a residual deleting-capability
    /// seam (e.g. across a mutation call) drove a removal the checker could not
    /// see; reject rather than silently skip the edge. A drop when the referencing
    /// row is itself deleted is harmless (the whole row vanishes), so it mirrors
    /// the `restrict`/patch rule of ignoring effects on deleted rows.
    fn check_undecided(&self, deletes: &BTreeSet<RowRef>) -> Result<(), DeleteError> {
        for edge in &self.edges {
            if edge.policy == DeletePolicy::Undecided
                && deletes.contains(&edge.to)
                && !deletes.contains(&edge.from)
            {
                return Err(DeleteError::DanglingUndecided {
                    referencing: Box::new(edge.from.clone()),
                    field: edge.field.clone(),
                    target: Box::new(edge.to.clone()),
                });
            }
        }
        Ok(())
    }

    /// §21.1: gather `none`-clear and `= patch` effects on surviving rows,
    /// combining disjoint or equal assignments and rejecting a conflict. Patches
    /// to a row that is itself deleted are ignored.
    fn collect_patches(
        &self,
        deletes: &BTreeSet<RowRef>,
    ) -> Result<BTreeMap<RowRef, BTreeMap<String, Value>>, DeleteError> {
        let mut patches: BTreeMap<RowRef, BTreeMap<String, Value>> = BTreeMap::new();
        for edge in &self.edges {
            if !deletes.contains(&edge.to) || deletes.contains(&edge.from) {
                continue;
            }
            let assignments = match &edge.policy {
                DeletePolicy::Clear => vec![(edge.field.clone(), Value::None)],
                DeletePolicy::Patch(assignments) => assignments.clone(),
                // Row deletions, set-member drops, and undecided backstop edges are
                // not field assignments (an undecided edge that reaches a live
                // target is already rejected by `check_undecided`).
                DeletePolicy::Restrict
                | DeletePolicy::Cascade
                | DeletePolicy::DropMember(_)
                | DeletePolicy::Undecided => continue,
            };
            let row_patch = patches.entry(edge.from.clone()).or_default();
            for (field, value) in assignments {
                match row_patch.get(&field) {
                    Some(existing) if *existing != value => {
                        return Err(DeleteError::ConflictingPatch {
                            row: Box::new(edge.from.clone()),
                            field,
                        });
                    }
                    _ => {
                        row_patch.insert(field, value);
                    }
                }
            }
        }
        Ok(patches)
    }

    /// §5.6/§21.1: gather the set-member drops a `cascade` (or `none`/clear) on a
    /// `$set`-of-`$ref` member induces. A drop applies only when the target is
    /// deleted and the referencing row survives; a drop onto a row that is itself
    /// deleted is redundant (the whole set vanishes with the row) and skipped —
    /// mirroring the patch rule. The referencing row keeps its identity; only its
    /// membership of the deleted target is removed, so a drop never propagates
    /// through the cascade closure.
    fn collect_member_removals(
        &self,
        deletes: &BTreeSet<RowRef>,
    ) -> BTreeMap<RowRef, BTreeMap<String, Vec<Value>>> {
        let mut removals: BTreeMap<RowRef, BTreeMap<String, Vec<Value>>> = BTreeMap::new();
        for edge in &self.edges {
            let DeletePolicy::DropMember(member) = &edge.policy else { continue };
            if !deletes.contains(&edge.to) || deletes.contains(&edge.from) {
                continue;
            }
            removals
                .entry(edge.from.clone())
                .or_default()
                .entry(edge.field.clone())
                .or_default()
                .push(member.clone());
        }
        removals
    }

    /// Apply a plan atomically (§21.1): remove every deleted row, apply each
    /// surviving-row patch, then drop each removed set member from surviving rows.
    pub fn apply(&mut self, plan: &DeletionPlan) {
        for row in &plan.deletes {
            self.rows.remove(row);
        }
        for (row, patch) in &plan.patches {
            if let Some(fields) = self.rows.get_mut(row) {
                for (field, value) in patch {
                    fields.insert(field.clone(), value.clone());
                }
            }
        }
        for (row, field_removals) in &plan.member_removals {
            let Some(fields) = self.rows.get_mut(row) else { continue };
            for (field, members) in field_removals {
                if let Some(Value::Set(set)) = fields.get_mut(field) {
                    for member in members {
                        set.remove(member);
                    }
                }
            }
        }
    }
}

/// The computed effect of a deletion (§21.1): the rows removed and the patches
/// applied to surviving rows.
#[derive(Debug, Clone)]
pub struct DeletionPlan {
    deletes: BTreeSet<RowRef>,
    patches: BTreeMap<RowRef, BTreeMap<String, Value>>,
    member_removals: BTreeMap<RowRef, BTreeMap<String, Vec<Value>>>,
}

impl DeletionPlan {
    /// The rows this plan removes from live state.
    #[must_use]
    pub fn deletes(&self) -> &BTreeSet<RowRef> {
        &self.deletes
    }

    /// The surviving-row patches this plan applies.
    #[must_use]
    pub fn patches(&self) -> &BTreeMap<RowRef, BTreeMap<String, Value>> {
        &self.patches
    }

    /// The set-member drops this plan applies to surviving rows (§5.6/§21.1): for
    /// each row, the members to remove from each of its `$set`-of-`$ref` fields.
    #[must_use]
    pub fn member_removals(&self) -> &BTreeMap<RowRef, BTreeMap<String, Vec<Value>>> {
        &self.member_removals
    }
}

/// Why a deletion or erasure could not proceed (§21.1/§21.2/§21.3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeleteError {
    /// A `restrict` ref from a surviving row blocks the deletion (§21.1). Boxed
    /// so the error stays small on the common `Ok` path.
    #[error("row {referencing:?} still references {target:?} via `{field}` (restrict)")]
    Restricted {
        /// The surviving referencing row.
        referencing: Box<RowRef>,
        /// The referencing field.
        field: String,
        /// The target that cannot be deleted.
        target: Box<RowRef>,
    },
    /// Two `= patch` effects assign different values to the same field (§21.1).
    #[error("conflicting `$on_delete` patches on `{field}` of {row:?}")]
    ConflictingPatch {
        /// The surviving row.
        row: Box<RowRef>,
        /// The conflicting field.
        field: String,
    },
    /// An inbound ref with an UNDECIDED `$on_delete` would be left dangling by
    /// its target's removal (§22.1/§21.1). A fail-closed backstop for the residual
    /// deleting-capability seam the static §21.1 gate cannot see; boxed so the
    /// error stays small on the common `Ok` path.
    #[error("row {referencing:?} references removed row {target:?} via `{field}` with an undecided `$on_delete`")]
    DanglingUndecided {
        /// The surviving referencing row.
        referencing: Box<RowRef>,
        /// The referencing field.
        field: String,
        /// The removed target the ref would dangle at.
        target: Box<RowRef>,
    },
    /// The erased occurrence has no retained payload to scrub.
    #[error("no retained payload for occurrence `{0}`")]
    NoPayload(String),
    /// A reinsertion's extract content hash did not verify (§21.3).
    #[error("extract content hash does not verify")]
    ExtractHashMismatch,
    /// A requested occurrence no longer bears the exact expected stub (§21.3).
    #[error("occurrence `{0}` no longer bears the expected digest stub")]
    StubMismatch(String),
}

/// A retained-history occurrence identity (a leaf position in history).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Occurrence(String);

impl Occurrence {
    /// Build an occurrence identity.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The occurrence id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The content of one retained history leaf: either its payload, or a digest
/// stub that preserves the leaf hash after erasure (§21.2 step 3).
#[derive(Debug, Clone, PartialEq, Eq)]
enum LeafContent {
    Payload(Value),
    Stub(String),
}

/// Retained history as a map of occurrences to leaf content (§21). A scrubbed
/// occurrence holds a stub, so its payload is unobservable while its verifiable
/// structure (the leaf hash) remains.
#[derive(Debug, Clone, Default)]
pub struct Erasure {
    leaves: BTreeMap<Occurrence, LeafContent>,
}

impl Erasure {
    /// A fresh empty history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a retained payload for `occurrence`.
    pub fn record(&mut self, occurrence: Occurrence, payload: Value) {
        self.leaves.insert(occurrence, LeafContent::Payload(payload));
    }

    /// The retained payload of `occurrence`, or `None` when it is absent or has
    /// been scrubbed to a stub — the "unobservable in history" observation
    /// (§21.2).
    #[must_use]
    pub fn payload(&self, occurrence: &Occurrence) -> Option<&Value> {
        match self.leaves.get(occurrence) {
            Some(LeafContent::Payload(value)) => Some(value),
            _ => None,
        }
    }

    /// The digest stub of `occurrence`, if it has been scrubbed (§21.2 step 3).
    #[must_use]
    pub fn stub(&self, occurrence: &Occurrence) -> Option<&str> {
        match self.leaves.get(occurrence) {
            Some(LeafContent::Stub(hash)) => Some(hash.as_str()),
            _ => None,
        }
    }

    /// A deterministic replay over retained history: the occurrences whose
    /// payload is observable. A scrubbed occurrence contributes no payload, so
    /// erased data is unobservable on replay just as in a live view (§21.2).
    #[must_use]
    pub fn replay_payloads(&self) -> BTreeMap<Occurrence, Value> {
        self.leaves
            .iter()
            .filter_map(|(occ, content)| match content {
                LeafContent::Payload(value) => Some((occ.clone(), value.clone())),
                LeafContent::Stub(_) => None,
            })
            .collect()
    }

    /// Erase the retained payloads of `occurrences` (§21.2): create a durable
    /// [`Extract`], replace each leaf with a digest stub of the same leaf hash,
    /// and return the extract. A missing payload rejects the whole operation.
    pub fn erase(&mut self, occurrences: &[Occurrence]) -> Result<Extract, DeleteError> {
        let mut payloads = BTreeMap::new();
        for occurrence in occurrences {
            match self.leaves.get(occurrence) {
                Some(LeafContent::Payload(value)) => {
                    payloads.insert(occurrence.clone(), value.clone());
                }
                _ => return Err(DeleteError::NoPayload(occurrence.0.clone())),
            }
        }
        // Replace each scrubbed occurrence with a stub of its leaf hash so
        // retained-history and artifact checksums still verify (§21.2 step 5).
        for (occurrence, payload) in &payloads {
            let hash = leaf_hash(payload);
            self.leaves.insert(occurrence.clone(), LeafContent::Stub(hash));
        }
        Ok(Extract { hash: extract_hash(&payloads), payloads })
    }

    /// Reinsert an extract's bytes (§21.3): verify the extract content hash and,
    /// for each requested occurrence, that the current leaf still bears the
    /// exact expected stub, then restore the payload. One mismatch rejects the
    /// whole reinsertion, so a second reinsertion (whose leaves are no longer
    /// stubs) fails.
    pub fn reinsert(&mut self, extract: &Extract) -> Result<(), DeleteError> {
        if extract_hash(&extract.payloads) != extract.hash {
            return Err(DeleteError::ExtractHashMismatch);
        }
        for (occurrence, payload) in &extract.payloads {
            let expected = leaf_hash(payload);
            if self.stub(occurrence) != Some(expected.as_str()) {
                return Err(DeleteError::StubMismatch(occurrence.0.clone()));
            }
        }
        for (occurrence, payload) in &extract.payloads {
            self.leaves.insert(occurrence.clone(), LeafContent::Payload(payload.clone()));
        }
        Ok(())
    }
}

/// The durable extract an erasure produces (§21.2 step 6): the scrubbed payloads
/// and their content hash, required to reinsert (§21.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extract {
    hash: String,
    payloads: BTreeMap<Occurrence, Value>,
}

impl Extract {
    /// The extract content hash (§21.3).
    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// A tampered copy of this extract whose content no longer matches its hash
    /// (§21.3): the reinsertion of such an extract must reject.
    #[must_use]
    pub fn tampered(&self, occurrence: &Occurrence, replacement: Value) -> Self {
        let mut payloads = self.payloads.clone();
        payloads.insert(occurrence.clone(), replacement);
        // The hash is the honest hash over the original payloads, so the
        // tampered content no longer verifies against it.
        Self { hash: self.hash.clone(), payloads }
    }
}

/// The leaf hash of a payload: the lowercase-hex SHA-512 of its canonical wire
/// form (§21.2 "the same logical leaf hash").
fn leaf_hash(payload: &Value) -> String {
    BlobIntegrity::digest_hex(payload.to_canonical_json_string().as_bytes())
}

/// The content hash of an extract: a stable digest over each occurrence and its
/// payload leaf hash (§21.2 step 4 / §21.3).
fn extract_hash(payloads: &BTreeMap<Occurrence, Value>) -> String {
    let mut material = String::new();
    for (occurrence, payload) in payloads {
        material.push_str(occurrence.as_str());
        material.push('\u{1f}');
        material.push_str(&leaf_hash(payload));
        material.push('\u{1e}');
    }
    BlobIntegrity::digest_hex(material.as_bytes())
}
