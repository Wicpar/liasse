//! D6: reconciling live subscriptions through a commit (§12.3, §12.6).
//!
//! After the host admits a call and sweeps its own subscriptions, this layer
//! reconstructs the coherent §12.2 downstream frames — with no runtime `ViewDelta`.
//! For each still-authorized subscription on the calling connection it re-projects
//! the recomputed authorized view to wire rows and diffs them against the client's
//! retained wire snapshot ([`diff_rows`]), enqueuing the patch on the SSE stream
//! *before* the committed reply returns. A commit is also an outgoing frontier for
//! every peer connection, so a peer's lost-authority subscription is closed here too
//! — but its rows are not advanced (a peer sees new rows no sooner than its own next
//! commit, §12.3).

use liasse_store::InstanceStore;
use liasse_surface::{CommitSeq, ViewRow};
use liasse_wire::{CloseReason, ConnectionToken, Downstream, Sub};

use super::frames::{diff_rows, project_rows};
use super::registry::ConnState;
use super::ConnectCore;

impl<S: InstanceStore> ConnectCore<S> {
    /// Reconcile every subscription through a commit (§12.3): the caller's advance and
    /// patch at its new frontier, every peer's lost-authority subscription closes at
    /// the outgoing frontier.
    pub(super) fn reconcile_after_commit(&mut self, token: &ConnectionToken, commit: CommitSeq) {
        let caller_seq = self.frontier_seq(token);
        self.sweep(token, caller_seq, true);
        let peers: Vec<ConnectionToken> =
            self.connections.keys().filter(|c| *c != token).cloned().collect();
        for peer in peers {
            self.sweep(&peer, commit.get(), false);
        }
    }

    /// Sweep one connection at frontier `seq`. The caller's subscriptions are
    /// re-projected and patched; a peer's are only closed on authority loss.
    fn sweep(&mut self, token: &ConnectionToken, seq: u64, caller: bool) {
        let conn = token.as_str().to_owned();
        for sub in self.connections.get(token).map(ConnState::sub_ids).unwrap_or_default() {
            let closed = self
                .connections
                .get(token)
                .and_then(|state| state.sub(&sub))
                .is_none_or(|s| s.closed);
            if closed {
                continue;
            }
            if self.host.close_reason(&conn, sub.as_str()).is_some() {
                self.emit_close(token, &sub, seq, CloseReason::Unauthorized);
                continue;
            }
            if !caller {
                continue;
            }
            let scalar = self
                .connections
                .get(token)
                .and_then(|state| state.sub(&sub))
                .is_some_and(|s| s.scalar);
            if scalar {
                self.advance_scalar(token, &conn, &sub, seq);
            } else {
                self.advance_rows(token, &conn, &sub, seq);
            }
        }
    }

    /// Advance a row-stream subscription: diff the retained wire snapshot against the
    /// re-projected authorized rows and enqueue the §12.2 patch (D6).
    fn advance_rows(&mut self, token: &ConnectionToken, conn: &str, sub: &Sub, seq: u64) {
        let windowed = self
            .connections
            .get(token)
            .and_then(|state| state.sub(sub))
            .is_some_and(|s| s.windowed);
        let rows = self.current_rows(conn, sub, windowed);
        let minter = self.minter.as_ref();
        let Some(state) = self.connections.get_mut(token) else { return };
        let new_snapshot = project_rows(state, minter, sub, &rows);
        let prior = state.sub(sub).map(|s| s.snapshot.clone()).unwrap_or_default();
        let ops = diff_rows(&prior, &new_snapshot);
        if let Some(sub_state) = state.sub_mut(sub) {
            sub_state.snapshot = new_snapshot;
        }
        if !ops.is_empty() {
            let ft = state.nonce().frontier(minter, seq);
            state.outbound_mut().enqueue(ft, seq, Downstream::Patch { sub: sub.clone(), ops });
        }
    }

    /// Advance a scalar subscription: emit the new value only when it changed (§7.5).
    fn advance_scalar(&mut self, token: &ConnectionToken, conn: &str, sub: &Sub, seq: u64) {
        let value = self.host.read_view(conn, sub.as_str()).and_then(|v| v.scalar().map(|x| x.to_wire()));
        let minter = self.minter.as_ref();
        let Some(state) = self.connections.get_mut(token) else { return };
        let Some(value) = value else { return };
        let changed = state.sub(sub).is_none_or(|s| s.scalar_value.as_ref() != Some(&value));
        if changed {
            if let Some(sub_state) = state.sub_mut(sub) {
                sub_state.scalar_value = Some(value.clone());
            }
            let ft = state.nonce().frontier(minter, seq);
            state.outbound_mut().enqueue(ft, seq, Downstream::Scalar { sub: sub.clone(), value });
        }
    }

    /// Re-project one subscription's current state as a fresh `init`/`scalar` at
    /// `seq` — the fresh-init resume path (§12.2). A closed subscription is skipped.
    pub(super) fn reproject_init(&mut self, token: &ConnectionToken, sub: &Sub, seq: u64) {
        let Some((windowed, scalar, closed)) = self
            .connections
            .get(token)
            .and_then(|state| state.sub(sub))
            .map(|s| (s.windowed, s.scalar, s.closed))
        else {
            return;
        };
        if closed {
            return;
        }
        let conn = token.as_str().to_owned();
        if scalar {
            let value = self.host.read_view(&conn, sub.as_str()).and_then(|v| v.scalar().map(|x| x.to_wire()));
            let minter = self.minter.as_ref();
            let Some(state) = self.connections.get_mut(token) else { return };
            if let Some(value) = value {
                if let Some(sub_state) = state.sub_mut(sub) {
                    sub_state.scalar_value = Some(value.clone());
                }
                let ft = state.nonce().frontier(minter, seq);
                state.outbound_mut().enqueue(ft, seq, Downstream::Scalar { sub: sub.clone(), value });
            }
            return;
        }
        let rows = self.current_rows(&conn, sub, windowed);
        let minter = self.minter.as_ref();
        let Some(state) = self.connections.get_mut(token) else { return };
        let snapshot = project_rows(state, minter, sub, &rows);
        if let Some(sub_state) = state.sub_mut(sub) {
            sub_state.snapshot = snapshot.clone();
        }
        let ft = state.nonce().frontier(minter, seq);
        state.outbound_mut().enqueue(ft, seq, Downstream::Init { sub: sub.clone(), rows: snapshot });
    }

    /// The authorized rows a subscription currently tracks — its window slice, or the
    /// full view — cloned out of the host so the connection state can then be mutated.
    pub(super) fn current_rows(&self, conn: &str, sub: &Sub, windowed: bool) -> Vec<ViewRow> {
        if windowed {
            self.host.read_window(conn, sub.as_str()).map(<[ViewRow]>::to_vec).unwrap_or_default()
        } else {
            self.host.read_view(conn, sub.as_str()).map(|v| v.rows().to_vec()).unwrap_or_default()
        }
    }

    /// Emit a terminal `close` for a subscription and mark it closed (§12.2).
    pub(super) fn emit_close(&mut self, token: &ConnectionToken, sub: &Sub, seq: u64, reason: CloseReason) {
        let minter = self.minter.as_ref();
        let Some(state) = self.connections.get_mut(token) else { return };
        if state.sub(sub).is_some_and(|s| s.closed) {
            return;
        }
        if let Some(sub_state) = state.sub_mut(sub) {
            sub_state.closed = true;
        }
        let ft = state.nonce().frontier(minter, seq);
        state.outbound_mut().enqueue(ft, seq, Downstream::Close { sub: sub.clone(), reason });
    }
}
