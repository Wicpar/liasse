//! The total order a sorted view fixes over its rows (§7.3, §8/Annex B.5).
//!
//! A `$sort` compares rows lexicographically by successive keys, reversing each
//! descending key, and appends occurrence identity ([`RowId`]) as the final
//! tiebreaker (§8, Annex B.5). [`SortOrder`] captures exactly that ordering as the
//! ordered per-key directions, so every consumer goes through one comparator: the
//! evaluator that sorts the rows (`order_rows`) and a bounded window that must
//! partition rows at a frozen gap coordinate (§12.2). Because both compare through
//! the same [`SortOrder::compare`], the window can never disagree with the
//! evaluator on a key's direction.

use std::cmp::Ordering;

use liasse_value::Value;

use crate::env::RowId;
use crate::typed::SortKey;

/// The direction and priority of a sorted view's keys (§7.3): one flag per key,
/// highest priority first, `true` for a descending key. Its length is the arity of
/// the `$sort` tuple each row carries. A view with no `$sort` is unordered — its
/// rows fall back to the pure occurrence-identity order (§8/Annex B.5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SortOrder {
    /// `descending[i]` reverses key `i`'s comparison (§7.3).
    descending: Vec<bool>,
}

impl SortOrder {
    /// The order of a view with no `$sort`: rows compare by occurrence identity
    /// alone (§8/Annex B.5).
    #[must_use]
    pub fn unordered() -> Self {
        Self::default()
    }

    /// The order fixed by the `keys` of the projection that produced a view's rows
    /// (§7.3): the per-key descending flags in priority order.
    #[must_use]
    pub(crate) fn from_keys(keys: &[SortKey]) -> Self {
        Self { descending: keys.iter().map(|key| key.descending).collect() }
    }

    /// Compare two rows in this total order (§7.3, §8/Annex B.5): successive sort
    /// keys with each descending key reversed, then ascending occurrence identity
    /// as the final tiebreak. A missing key component compares equal, so a shorter
    /// tuple defers to the tiebreak — the same reduction the evaluator makes. This
    /// is the single comparator both the evaluator's row ordering and a bounded
    /// window's gap partition go through, so neither can drift on direction.
    ///
    /// Optional `none` sorts last ascending / first descending because
    /// [`Value::None`] is the Annex B.2 order maximum and reversal flips it.
    #[must_use]
    pub fn compare(
        &self,
        a_keys: &[Value],
        a_id: &RowId,
        b_keys: &[Value],
        b_id: &RowId,
    ) -> Ordering {
        for (index, descending) in self.descending.iter().enumerate() {
            let ordering = match (a_keys.get(index), b_keys.get(index)) {
                (Some(a), Some(b)) => a.cmp(b),
                _ => Ordering::Equal,
            };
            let ordering = if *descending { ordering.reverse() } else { ordering };
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        a_id.cmp(b_id)
    }

    /// Whether `(a_keys, a_id)` sorts strictly before `(b_keys, b_id)` in this
    /// order — the partition predicate a bounded window uses to place its frozen
    /// gap coordinate (§12.2): the window begins at the first row that is *not*
    /// before the coordinate.
    #[must_use]
    pub fn is_before(
        &self,
        a_keys: &[Value],
        a_id: &RowId,
        b_keys: &[Value],
        b_id: &RowId,
    ) -> bool {
        self.compare(a_keys, a_id, b_keys, b_id) == Ordering::Less
    }
}
