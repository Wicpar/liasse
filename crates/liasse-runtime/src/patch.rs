//! The §12.2 live-view patch vocabulary and the ordered diff between two view
//! results.
//!
//! A live subscription delivers `init(frontier, rows)` then ordered `patch`es
//! (SPEC.md §12.2). This module models a patch as an ordered [`Vec<PatchOp>`] over
//! the five §12.2 operations and computes, from a prior and a next result, a
//! sequence that — applied in listed order — carries the client's prior ordered
//! result to the next one EXACTLY, order included:
//!
//! > After applying every operation, the client result MUST equal the authorized
//! > declared view at the new frontier.
//!
//! Positions (`at`, `to`) are zero-based indices in the CURRENT (mid-application)
//! result, so [`diff`] accounts for how earlier operations shift later positions.

use std::collections::BTreeMap;

use liasse_expr::RowId;
use liasse_value::Value;

use crate::view::ViewRow;

/// One §12.2 live-view patch operation. A patch is an ordered `Vec<PatchOp>`; a
/// frontier-only patch (nothing changed) is the empty sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchOp {
    /// `insert { $at, $id, $value }` — a new occurrence enters at zero-based
    /// position `at` in the current result. `row` carries the occurrence identity
    /// (`row.id()`) and its exposed value.
    Insert { at: usize, row: ViewRow },
    /// `remove { $id }` — the occurrence leaves the result.
    Remove { id: RowId },
    /// `move { $id, $to }` — the occurrence changes to zero-based position `to` in
    /// the current result. Its exposed value is unchanged.
    Move { id: RowId, to: usize },
    /// `update { $id, $value }` — the occurrence's exposed value changes; identity
    /// AND position are preserved (a reposition is a separate [`PatchOp::Move`]).
    Update { row: ViewRow },
    /// `rekey { $id, $key }` — the occurrence's exposed key changes while its
    /// occurrence and row incarnation are preserved (§5.4, §12.2).
    ///
    /// [`diff`] never SYNTHESIZES this op. A [`ViewRow`]'s identity is its
    /// key-derived [`RowId`] (Annex D.1), so an atomic rekey presents as a
    /// distinct identity and diffs as [`PatchOp::Remove`] + [`PatchOp::Insert`],
    /// which still yields the correct ordered result. Emitting `rekey` needs a
    /// rekey-stable occurrence identity the flat, key-derived result does not
    /// carry — the runtime seam already documented for a window anchor crossing a
    /// rekey. The op is part of the vocabulary for a layer that tracks that
    /// continuity.
    Rekey { id: RowId, key: Value },
}

impl PatchOp {
    /// The occurrence identity this operation affects.
    #[must_use]
    pub fn id(&self) -> &RowId {
        match self {
            Self::Insert { row, .. } | Self::Update { row } => row.id(),
            Self::Remove { id } | Self::Move { id, .. } | Self::Rekey { id, .. } => id,
        }
    }
}

/// Compute an ordered §12.2 patch that, applied in order to `prev`'s rows, yields
/// `next`'s rows EXACTLY — same occurrences, same exposed values, same order
/// (§12.2). The sequence is deterministic and correct, not necessarily minimal:
///
/// 1. a pass over `prev`: `remove` every departed occurrence and `update` every
///    survivor whose exposed value changed (position-preserving);
/// 2. a left-to-right pass over `next`: place each target occurrence at its
///    position in the current result, emitting `insert { at }` for a new
///    occurrence and `move { to }` for a survivor that is out of place.
///
/// Positions are interpreted in the working result as it stands when each op
/// runs, exactly as a client applies them. A key change is a distinct identity,
/// so it falls out of (1)+(3) as a remove of the old key and an insert of the new
/// one (see [`PatchOp::Rekey`]).
pub(crate) fn diff(prev: &[ViewRow], next: &[ViewRow]) -> Vec<PatchOp> {
    let next_by_id: BTreeMap<&RowId, &ViewRow> = next.iter().map(|row| (row.id(), row)).collect();
    let mut ops = Vec::new();

    // (1) `remove` every departed occurrence; `update` every survivor whose
    // exposed value changed (position-preserving). Both target an occurrence by
    // identity, so the two kinds are independent of position and each other.
    // `working` records the surviving occurrences in `prev` order.
    let mut working: Vec<RowId> = Vec::new();
    for row in prev {
        match next_by_id.get(row.id()) {
            None => ops.push(PatchOp::Remove { id: row.id().clone() }),
            Some(&after) => {
                working.push(row.id().clone());
                if !row.same_value(after) {
                    ops.push(PatchOp::Update { row: after.clone() });
                }
            }
        }
    }

    // (2) Left-to-right placement. Invariant: after index `i`, working[0..=i]
    // equals next[0..=i]. A survivor out of place therefore sits at some index
    // > i (its settled predecessors already fill [0..i]), so a move to `i` never
    // disturbs the settled prefix; a new occurrence inserts at `i`.
    for (index, target) in next.iter().enumerate() {
        let target_id = target.id();
        if working.get(index).is_some_and(|id| id == target_id) {
            continue;
        }
        match working.iter().position(|id| id == target_id) {
            Some(current) => {
                let id = working.remove(current);
                working.insert(index, id);
                ops.push(PatchOp::Move { id: target_id.clone(), to: index });
            }
            None => {
                working.insert(index, target_id.clone());
                ops.push(PatchOp::Insert { at: index, row: target.clone() });
            }
        }
    }

    ops
}
