//! Operation identifiers, deduplication, and retained status (SPEC.md §12.3).
//!
//! An external call MAY carry a high-entropy operation identifier. Its scope is
//! the application, the public or scoped-role target, the selected authenticator
//! when present, and the identifier (§12.3). Reusing that scoped identifier with
//! an *equivalent* request re-observes the retained outcome (at-most-once
//! execution); reusing it with different request metadata rejects the call. A
//! call with no identifier is a new operation every time.
//!
//! What counts as "equivalent" is compared over the full resolved request model
//! — the mutation, its receiver key, and every supplied argument. SPEC-ISSUES
//! item 6 records that the spec does not pin whether an *unknown* argument member
//! is ignored, which would make equivalence ambiguous; this layer compares
//! arguments verbatim, so differing unknown members read as different requests
//! (the conservative, never-collide choice) rather than silently deduplicating.

use std::collections::BTreeMap;

use liasse_runtime::{CommitSeq, Value};

use crate::outcome::SurfaceOutcome;

/// The scope key of a retained operation (§12.3): the surface target, the
/// selected authenticator (when present), and the identifier. Two submissions
/// share an operation exactly when these three agree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OperationKey {
    target: String,
    auth: Option<String>,
    id: String,
}

impl OperationKey {
    /// The scope key for identifier `id` on `target`, selected under `auth`.
    #[must_use]
    pub fn new(target: impl Into<String>, auth: Option<String>, id: impl Into<String>) -> Self {
        Self { target: target.into(), auth, id: id.into() }
    }
}

/// The resolved request model an operation is compared by (§12.3 "an equivalent
/// request"): the mutation, the selected receiver key, and the supplied
/// arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestModel {
    mutation: String,
    receiver: Vec<Value>,
    args: BTreeMap<String, Value>,
}

impl RequestModel {
    /// The request model of a call invoking `mutation` on the `receiver` key
    /// with `args`.
    #[must_use]
    pub fn new(
        mutation: impl Into<String>,
        receiver: Vec<Value>,
        args: BTreeMap<String, Value>,
    ) -> Self {
        Self { mutation: mutation.into(), receiver, args }
    }
}

/// The retained runtime status of an operation (§12.3). It is runtime metadata,
/// never application state or exported history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationStatus {
    /// The operation committed at `commit`; `frontier` covers it.
    Committed { frontier: CommitSeq, commit: CommitSeq },
    /// The operation changed nothing (§8.9); evaluated at `frontier`.
    Unchanged { frontier: CommitSeq },
    /// The operation was refused at admission.
    Rejected,
    /// No record is retained — never submitted, or expired by host policy.
    Unknown,
}

/// One retained operation: the request it executed and the outcome it produced.
struct OperationRecord {
    request: RequestModel,
    outcome: SurfaceOutcome,
}

/// What to do with a submission carrying an operation identifier.
pub enum Dedup<'a> {
    /// No prior record under this key: execute and record the outcome.
    Fresh,
    /// A prior record with an equivalent request: re-observe its outcome without
    /// re-executing (at-most-once).
    Replay(&'a SurfaceOutcome),
    /// A prior record with a different request: reject without executing, and
    /// leave the original record intact.
    Conflict,
}

/// The retained operation records for one host. Plain owned state — the future
/// executor drives it single-threaded, so no interior mutability is needed.
#[derive(Default)]
pub struct OperationLog {
    records: BTreeMap<OperationKey, OperationRecord>,
}

impl OperationLog {
    /// An empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide how to treat a submission of `request` under `key` (§12.3).
    #[must_use]
    pub fn decide(&self, key: &OperationKey, request: &RequestModel) -> Dedup<'_> {
        match self.records.get(key) {
            None => Dedup::Fresh,
            Some(record) if &record.request == request => Dedup::Replay(&record.outcome),
            Some(_) => Dedup::Conflict,
        }
    }

    /// Retain the outcome of a freshly executed operation. Only completed
    /// outcomes (committed, unchanged, rejected) are retained; a denial is not a
    /// §12.3 status and is dropped.
    pub fn record(&mut self, key: OperationKey, request: RequestModel, outcome: SurfaceOutcome) {
        if matches!(outcome, SurfaceOutcome::Denied(_)) {
            return;
        }
        self.records.insert(key, OperationRecord { request, outcome });
    }

    /// The retained status for `key`, or [`OperationStatus::Unknown`] (§12.3).
    #[must_use]
    pub fn status(&self, key: &OperationKey) -> OperationStatus {
        match self.records.get(key) {
            None => OperationStatus::Unknown,
            Some(record) => match &record.outcome {
                SurfaceOutcome::Committed { commit, .. } => {
                    OperationStatus::Committed { frontier: *commit, commit: *commit }
                }
                SurfaceOutcome::Unchanged { frontier, .. } => {
                    OperationStatus::Unchanged { frontier: *frontier }
                }
                SurfaceOutcome::Rejected(_) => OperationStatus::Rejected,
                SurfaceOutcome::Denied(_) => OperationStatus::Unknown,
            },
        }
    }
}
