//! Serial positions and the durable commit-log record (§22.3, §19.2).

use liasse_ident::{RowIncarnation, TransactionId};
use liasse_value::Value;

use crate::key::RowAddress;

/// A serial position in one instance's execution order (§22.3).
///
/// Positions are gapless and strictly monotone: [`CommitSeq::GENESIS`] is the
/// empty pre-history state, and each admitted commit takes the immediate
/// successor of the current head. The linear sequence of positions is the
/// implementation's declared acyclic precedence relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitSeq(u64);

impl CommitSeq {
    /// The pre-history position: no transition has committed yet.
    pub const GENESIS: Self = Self(0);

    /// The immediate successor position — the seat the next commit takes.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    /// Reconstruct the position whose underlying number is `n`, for a store
    /// reloading its own durable positions.
    ///
    /// A position is normally reachable only through [`CommitSeq::GENESIS`] and
    /// [`CommitSeq::next`], which is what makes the sequence gapless and monotone
    /// by construction. Reconstruction is the sole exception, and it carries an
    /// invariant the caller owns: every `n` passed here was minted by `next` on a
    /// prior run and persisted as a gapless serial (§22.3), so rebuilding it
    /// directly is faithful to that provenance. It exists so a reload is O(1) per
    /// position — the exact inverse of [`CommitSeq::get`] — instead of replaying
    /// `next` `n` times, which would make loading a store quadratic in its commit
    /// count. Code that mints *new* positions must still use `next`; this is only
    /// for reading positions the store itself already minted.
    #[must_use]
    pub const fn from_stored(n: u64) -> Self {
        Self(n)
    }

    /// The underlying position number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The outcome of committing a staged transition (§22.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The transition changed durable state and took the given serial position.
    Committed(CommitSeq),
    /// The transition staged nothing, so no commit was created and no position
    /// was consumed (§22.2: "A program producing no state change returns
    /// `unchanged` and creates no commit").
    Unchanged,
}

/// One resolved row operation as it is durably recorded, carrying the exact
/// incarnation so that replay reproduces identity without re-allocating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommittedRowOp {
    /// A fresh row: the store allocated `incarnation` at admission.
    Insert { address: RowAddress, incarnation: RowIncarnation, value: Value },
    /// A new payload for an existing row; incarnation and address unchanged.
    Update { address: RowAddress, incarnation: RowIncarnation, value: Value },
    /// Removal of a live row; its incarnation is recorded for auditability and
    /// so a lying replay can be detected.
    Delete { address: RowAddress, incarnation: RowIncarnation },
    /// An atomic rekey (§5.4): the row moves from `from` to `to`, keeping its
    /// incarnation, with `value` its payload at the new address.
    Rekey {
        from: RowAddress,
        to: RowAddress,
        incarnation: RowIncarnation,
        value: Value,
    },
}

/// One committed transition: the durable, replayable unit of the commit log.
///
/// It bundles every row operation admitted together at one serial position,
/// plus any definition/composition change and the shared cross-instance
/// transaction identity (§19.1). Applying the log's transitions in position
/// order reproduces committed state exactly (§19.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedTransition {
    seq: CommitSeq,
    ops: Vec<CommittedRowOp>,
    transaction: Option<TransactionId>,
}

impl CommittedTransition {
    /// Assemble a committed transition record.
    #[must_use]
    pub fn new(
        seq: CommitSeq,
        ops: Vec<CommittedRowOp>,
        transaction: Option<TransactionId>,
    ) -> Self {
        Self { seq, ops, transaction }
    }

    /// The serial position this transition occupies.
    #[must_use]
    pub fn seq(&self) -> CommitSeq {
        self.seq
    }

    /// The row operations admitted together, in application order.
    #[must_use]
    pub fn ops(&self) -> &[CommittedRowOp] {
        &self.ops
    }

    /// The shared transaction identity when this transition is part of a
    /// cross-instance commit (§19.1).
    #[must_use]
    pub fn transaction(&self) -> Option<&TransactionId> {
        self.transaction.as_ref()
    }
}
