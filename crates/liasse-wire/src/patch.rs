//! The wire form of the §12.2 patch vocabulary and the single, index-checked
//! function that applies an ordered patch to a prior row set.
//!
//! `liasse-runtime` owns [`diff`](../../liasse-runtime), which computes the ordered
//! patch between two view results. Applying that patch existed only as five
//! duplicated copies inside `liasse-surface`'s tests. This module is the ONE
//! application function they now share, so the client, the server codec, and the
//! conformance corpus agree on §12.2 apply semantics by construction.
//!
//! The operations mirror the runtime enum one-for-one; the `at`/`to` positions are
//! zero-based indices in the CURRENT (mid-application) result, exactly as the
//! runtime's `diff` interprets them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::row::WireRow;
use crate::token::Occ;

/// One §12.2 patch operation, keyed by opaque occurrence token. On the wire the
/// variant is tagged by `op` and the occurrence token is the member `id`, matching
/// [`WireRow`]'s `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PatchOp {
    /// `insert { op, at, id, value }` — a new occurrence enters at zero-based
    /// position `at` in the current result.
    Insert {
        /// The zero-based position in the current result the row enters at.
        at: usize,
        /// The new occurrence's token.
        #[serde(rename = "id")]
        occ: Occ,
        /// The new occurrence's exposed value.
        value: Value,
    },
    /// `remove { op, id }` — the occurrence leaves the result.
    Remove {
        /// The departing occurrence's token.
        #[serde(rename = "id")]
        occ: Occ,
    },
    /// `move { op, id, to }` — the occurrence moves to zero-based position `to` in
    /// the current result; its value is unchanged.
    Move {
        /// The moving occurrence's token.
        #[serde(rename = "id")]
        occ: Occ,
        /// The zero-based destination position in the current result.
        to: usize,
    },
    /// `update { op, id, value }` — the occurrence's exposed value changes; its
    /// identity and position are preserved.
    Update {
        /// The updated occurrence's token.
        #[serde(rename = "id")]
        occ: Occ,
        /// The occurrence's new exposed value.
        value: Value,
    },
    /// `rekey { op, id, key }` — the occurrence's exposed key changes while its
    /// occurrence identity is preserved (§5.4, §12.2). [`diff`](../../liasse-runtime)
    /// never synthesizes this — a flat key-derived result renders a key change as
    /// `remove` + `insert` — so it exists for a layer that tracks a rekey-stable
    /// occurrence. [`apply`] therefore only checks the occurrence is present and
    /// preserves it; the new value that accompanies the new key rides an `update`.
    Rekey {
        /// The occurrence whose exposed key changes.
        #[serde(rename = "id")]
        occ: Occ,
        /// The new exposed key.
        key: Value,
    },
}

impl PatchOp {
    /// The occurrence token this operation affects.
    #[must_use]
    pub fn occ(&self) -> &Occ {
        match self {
            Self::Insert { occ, .. }
            | Self::Remove { occ }
            | Self::Move { occ, .. }
            | Self::Update { occ, .. }
            | Self::Rekey { occ, .. } => occ,
        }
    }
}

/// Why an ordered patch could not be applied to a given prior result. Every case
/// is a patch that does not describe a valid transition of `prev`; `apply` returns
/// it instead of panicking, so a malformed or hostile patch can never abort the
/// process.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    /// `remove`/`move`/`update`/`rekey` named an occurrence absent from the current
    /// result — the client and server disagree on membership.
    #[error("patch targets occurrence `{occ}` which is absent from the current result")]
    UnknownOccurrence {
        /// The token that resolved to no row.
        occ: Occ,
    },
    /// `insert`/`move` named a position past the end of the current result. A
    /// well-formed §12.2 patch never does: each position is read in the result as it
    /// stands when the operation runs, so it is at most its length.
    #[error("patch position {position} is out of range for a result of length {length}")]
    PositionOutOfRange {
        /// The requested zero-based position.
        position: usize,
        /// The length of the current result when the operation ran.
        length: usize,
    },
    /// `insert` named an occurrence already present — an occurrence token is unique
    /// within a subscription, so this is a corrupt or hostile patch.
    #[error("patch inserts occurrence `{occ}` which is already present")]
    DuplicateOccurrence {
        /// The token that was already present.
        occ: Occ,
    },
}

/// Apply an ordered §12.2 patch to `prev`, yielding the next result.
///
/// This is the single source of truth for §12.2 apply semantics. It mirrors
/// [`diff`](../../liasse-runtime)'s reading of positions: each `at`/`to` is an index
/// in the result AS IT STANDS when the operation runs, so applying `diff(prev,
/// next)` to `prev` reproduces `next` exactly — occurrences, values, and order. An
/// empty `ops` is the frontier-only patch (nothing changed) and returns a copy of
/// `prev`.
///
/// Every index is checked before use, so no input can panic: an out-of-range
/// position, an unknown occurrence, or a duplicate insert returns [`ApplyError`].
pub fn apply(prev: &[WireRow], ops: &[PatchOp]) -> Result<Vec<WireRow>, ApplyError> {
    let mut rows: Vec<WireRow> = prev.to_vec();
    for op in ops {
        match op {
            PatchOp::Remove { occ } => {
                let at = position(&rows, occ)?;
                rows.remove(at);
            }
            PatchOp::Update { occ, value } => {
                // `position` proved the index valid, so `get_mut` is `Some`; the
                // `if let` avoids a panicking index without a dead error branch.
                let at = position(&rows, occ)?;
                if let Some(row) = rows.get_mut(at) {
                    row.set_value(value.clone());
                }
            }
            PatchOp::Move { occ, to } => {
                let at = position(&rows, occ)?;
                let row = rows.remove(at);
                bounds(*to, rows.len())?;
                rows.insert(*to, row);
            }
            PatchOp::Insert { at, occ, value } => {
                if rows.iter().any(|row| row.occ() == occ) {
                    return Err(ApplyError::DuplicateOccurrence { occ: occ.clone() });
                }
                bounds(*at, rows.len())?;
                rows.insert(*at, WireRow::new(occ.clone(), value.clone()));
            }
            PatchOp::Rekey { occ, .. } => {
                // A rekey preserves the occurrence and its position; the value that
                // accompanies the new key rides an `update`, so applying it is a
                // presence check on the occurrence.
                position(&rows, occ)?;
            }
        }
    }
    Ok(rows)
}

/// The index of the row carrying `occ`, or [`ApplyError::UnknownOccurrence`].
fn position(rows: &[WireRow], occ: &Occ) -> Result<usize, ApplyError> {
    rows.iter()
        .position(|row| row.occ() == occ)
        .ok_or_else(|| ApplyError::UnknownOccurrence { occ: occ.clone() })
}

/// Check `position` names a valid insertion index into a result of `length`.
fn bounds(position: usize, length: usize) -> Result<(), ApplyError> {
    if position > length {
        return Err(ApplyError::PositionOutOfRange { position, length });
    }
    Ok(())
}
