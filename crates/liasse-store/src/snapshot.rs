//! A materialized read of committed state at one serial frontier.
//!
//! A [`Snapshot`] is the shared primitive behind three guarantees: a live-view
//! read of current state, a replay read at an earlier frontier, and the
//! reconstruction that proves the commit log is a faithful durable record
//! (§19.2, §22.7). It is built purely from committed transitions, so it is
//! backend-independent — every implementation folds the same log the same way,
//! which is what lets one conformance suite check them all.

use std::collections::BTreeMap;

use crate::commit::{CommitSeq, CommittedRowOp, CommittedTransition};
use crate::error::StoreError;
use crate::key::{CollectionPath, RowAddress};
use crate::row::StoredRow;

/// The complete committed state at one frontier: every live row keyed by its
/// address, in Annex B order (the [`RowAddress`] [`Ord`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    frontier: CommitSeq,
    rows: BTreeMap<RowAddress, StoredRow>,
}

impl Snapshot {
    /// The empty genesis snapshot.
    #[must_use]
    pub fn empty() -> Self {
        Self { frontier: CommitSeq::GENESIS, rows: BTreeMap::new() }
    }

    /// Replay committed transitions up to and including `frontier`, folding them
    /// into materialized state.
    ///
    /// Transitions with a position past `frontier` are ignored — this is exactly
    /// the snapshot-at-frontier semantics (a later commit is invisible to an
    /// earlier read). Transitions must be replayed in ascending position order;
    /// an op that cannot apply against the state built so far means the log is
    /// corrupt (its `before` state disagrees with the record).
    pub fn replay<'a>(
        transitions: impl IntoIterator<Item = &'a CommittedTransition>,
        frontier: CommitSeq,
    ) -> Result<Self, StoreError> {
        let mut rows: BTreeMap<RowAddress, StoredRow> = BTreeMap::new();
        for transition in transitions {
            if transition.seq() > frontier {
                continue;
            }
            for op in transition.ops() {
                Self::apply(&mut rows, op)?;
            }
        }
        Ok(Self { frontier, rows })
    }

    /// Materialize current state from a whole log (frontier = its head).
    pub fn materialize<'a>(
        transitions: impl IntoIterator<Item = &'a CommittedTransition>,
        head: CommitSeq,
    ) -> Result<Self, StoreError> {
        Self::replay(transitions, head)
    }

    /// The frontier this snapshot reflects.
    #[must_use]
    pub fn frontier(&self) -> CommitSeq {
        self.frontier
    }

    /// Read one row by its canonical address.
    #[must_use]
    pub fn row(&self, address: &RowAddress) -> Option<&StoredRow> {
        self.rows.get(address)
    }

    /// The direct rows of one collection, in Annex B key-ascending order (B.5).
    #[must_use]
    pub fn scan(&self, collection: &CollectionPath) -> Vec<(RowAddress, StoredRow)> {
        self.rows
            .iter()
            .filter(|(address, _)| collection.contains(address))
            .map(|(address, row)| (address.clone(), row.clone()))
            .collect()
    }

    /// Every row of the subtree rooted at `root` (excluding `root`) reached through
    /// the nested collections `steps`, in Annex B address order — the frontier read
    /// twin of [`crate::InstanceStore::scan_subtree`] (§7.6). A snapshot holds only
    /// live rows, so this is the same prefix range the [`MemoryStore`] oracle
    /// enumerates, with tombstoned intermediates absent but their live orphans kept.
    ///
    /// [`MemoryStore`]: crate::MemoryStore
    #[must_use]
    pub fn scan_subtree(&self, root: &RowAddress, steps: &[String]) -> Vec<(RowAddress, StoredRow)> {
        self.rows
            .iter()
            .filter(|(address, _)| address.descends_from(root, steps))
            .map(|(address, row)| (address.clone(), row.clone()))
            .collect()
    }

    /// The number of live rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the snapshot holds no live rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn apply(
        rows: &mut BTreeMap<RowAddress, StoredRow>,
        op: &CommittedRowOp,
    ) -> Result<(), StoreError> {
        match op {
            CommittedRowOp::Insert { address, incarnation, value } => {
                if rows.contains_key(address) {
                    return Err(StoreError::Corruption {
                        detail: format!("replayed insert over live row at `{}`", address.render()),
                    });
                }
                rows.insert(address.clone(), StoredRow::new(incarnation.clone(), value.clone()));
            }
            CommittedRowOp::Update { address, incarnation, value } => {
                if !rows.contains_key(address) {
                    return Err(StoreError::Corruption {
                        detail: format!("replayed update on absent row at `{}`", address.render()),
                    });
                }
                rows.insert(address.clone(), StoredRow::new(incarnation.clone(), value.clone()));
            }
            CommittedRowOp::Delete { address, .. } => {
                if rows.remove(address).is_none() {
                    return Err(StoreError::Corruption {
                        detail: format!("replayed delete on absent row at `{}`", address.render()),
                    });
                }
            }
            CommittedRowOp::Rekey { from, to, incarnation, value } => {
                if rows.remove(from).is_none() {
                    return Err(StoreError::Corruption {
                        detail: format!("replayed rekey from absent row at `{}`", from.render()),
                    });
                }
                if rows.contains_key(to) {
                    return Err(StoreError::Corruption {
                        detail: format!("replayed rekey onto live row at `{}`", to.render()),
                    });
                }
                rows.insert(to.clone(), StoredRow::new(incarnation.clone(), value.clone()));
            }
        }
        Ok(())
    }
}
