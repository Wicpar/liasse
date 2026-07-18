//! D6: reconstructing the §12.2 ordered patch from two wire snapshots.
//!
//! The runtime owns `patch::diff` over its internal `ViewRow`s, but the connect
//! layer never sees a `ViewDelta` — D6 keeps the surface API untouched. Instead it
//! retains, per subscription, the exact wire rows the client holds, and after each
//! commit diffs that snapshot against the freshly projected rows. Because the
//! occurrence token is a stable relabeling of the internal `RowId`, this diff over
//! [`Occ`]-keyed [`WireRow`]s is the runtime diff modulo that relabeling: applying
//! its ops (via [`liasse_wire::apply`]) to the prior snapshot reproduces the new one
//! EXACTLY — same occurrences, same values, same order — which is the §12.2 coherence
//! contract.
//!
//! The algorithm mirrors `patch::diff` one-for-one so the reconstruction matches the
//! semantics the conformance corpus already trusts: (1) a pass over `prev` emitting
//! `remove` for departures and `update` for value changes; (2) a left-to-right pass
//! over `next` placing each occurrence with `insert`/`move`. Positions are indices in
//! the working result as each op runs.

use liasse_surface::ViewRow;
use liasse_wire::{Occ, PatchOp, Sub, WireRow};

use crate::encode;
use crate::token::TokenMinter;

use super::registry::ConnState;

/// Project runtime view rows to wire rows, minting a stable occurrence token per row
/// within subscription `sub`. The occurrence bijection persists for the
/// subscription's life, so the token is stable and never reused (§12.2).
#[must_use]
pub fn project_rows(
    state: &mut ConnState,
    minter: &dyn TokenMinter,
    sub: &Sub,
    rows: &[ViewRow],
) -> Vec<WireRow> {
    rows.iter()
        .map(|row| {
            let occ = state.mint_occ(minter, sub, row.id());
            encode::wire_row(row, occ)
        })
        .collect()
}

/// The ordered §12.2 patch carrying `prev` to `next` exactly (occurrences, values,
/// order). An empty result is the frontier-only no-op.
#[must_use]
pub fn diff_rows(prev: &[WireRow], next: &[WireRow]) -> Vec<PatchOp> {
    let mut ops = Vec::new();

    // (1) `remove` departed occurrences; `update` survivors whose value changed.
    // `working` records the surviving occurrences in `prev` order.
    let mut working: Vec<Occ> = Vec::new();
    for row in prev {
        match next.iter().find(|candidate| candidate.occ() == row.occ()) {
            None => ops.push(PatchOp::Remove { occ: row.occ().clone() }),
            Some(after) => {
                working.push(row.occ().clone());
                if after.value() != row.value() {
                    ops.push(PatchOp::Update { occ: after.occ().clone(), value: after.value().clone() });
                }
            }
        }
    }

    // (2) Left-to-right placement: after index `i`, working[0..=i] equals next[0..=i].
    for (index, target) in next.iter().enumerate() {
        if working.get(index).is_some_and(|occ| occ == target.occ()) {
            continue;
        }
        match working.iter().position(|occ| occ == target.occ()) {
            Some(current) => {
                let occ = working.remove(current);
                working.insert(index, occ);
                ops.push(PatchOp::Move { occ: target.occ().clone(), to: index });
            }
            None => {
                working.insert(index, target.occ().clone());
                ops.push(PatchOp::Insert { at: index, occ: target.occ().clone(), value: target.value().clone() });
            }
        }
    }

    ops
}
