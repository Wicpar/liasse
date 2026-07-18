//! The connection registry: per-connection minting state, per-subscription
//! occurrence bijection, and the bounded outbound ring (§12).
//!
//! One [`ConnState`] is the whole server-side memory of one logical connection: the
//! [`ConnKeys`] (secret credential + public id) that bind its tokens, the dense
//! counter behind its occurrence capabilities, and the ring of frames it has emitted
//! (for `Last-Event-ID` replay).
//! Each [`SubState`] holds the RowId→[`Occ`] bijection that projects a subscription's
//! internal identities onto opaque tokens, and the *wire snapshot* — the exact rows
//! the client currently holds — that D6 diffs the recomputed view against, so a
//! coherent §12.2 patch is reconstructed without any runtime `ViewDelta`.

use std::collections::{BTreeMap, VecDeque};

use liasse_surface::RowId;
use liasse_wire::{Downstream, Ft, Occ, Sub, WireRow};

use crate::token::{ConnKeys, TokenMinter};

/// The whole server state of one logical connection.
pub struct ConnState {
    /// The connection's [`ConnKeys`]: its secret credential (also the registry key
    /// and wire [`ConnectionToken`](liasse_wire::ConnectionToken)) and the public id
    /// embedded in the ft/occ tokens it mints.
    keys: ConnKeys,
    /// The dense counter behind occurrence capabilities — monotone, so a token is
    /// never reused for a different occurrence even after a row leaves the view.
    occ_counter: u64,
    /// Reverse index for inbound anchor resolution: which subscription and internal
    /// row an occurrence token names (§12.2 concrete anchor).
    occ_index: BTreeMap<Occ, (Sub, RowId)>,
    /// The live subscriptions, by their client-chosen id.
    subs: BTreeMap<Sub, SubState>,
    /// The bounded downstream ring.
    outbound: Outbound,
    /// The §12.3 operation capabilities issued on this connection, mapped to the
    /// scope key that reads their retained status.
    operations: BTreeMap<liasse_wire::OperationId, liasse_surface::OperationKey>,
    /// Each bound authentication context's authenticator name (§11.8), so a role
    /// call's §12.3 scope key can be reconstructed for a status query.
    contexts: BTreeMap<String, String>,
}

impl ConnState {
    /// Open connection state under `keys`, buffering up to `capacity` outbound
    /// frames before backpressure.
    #[must_use]
    pub fn new(keys: ConnKeys, capacity: usize) -> Self {
        Self {
            keys,
            occ_counter: 0,
            occ_index: BTreeMap::new(),
            subs: BTreeMap::new(),
            outbound: Outbound::new(capacity),
            operations: BTreeMap::new(),
            contexts: BTreeMap::new(),
        }
    }

    /// The connection's [`ConnKeys`] (secret credential and public id).
    #[must_use]
    pub fn keys(&self) -> &ConnKeys {
        &self.keys
    }

    /// The bounded outbound ring.
    pub fn outbound_mut(&mut self) -> &mut Outbound {
        &mut self.outbound
    }

    /// Whether a resume from frontier `seq` is still fully retained.
    #[must_use]
    pub fn outbound_replayable(&self, seq: u64) -> bool {
        self.outbound.replayable(seq)
    }

    /// Record the authenticator that bound context `name` (§11.8).
    pub fn record_bound_context(&mut self, name: String, auth: String) {
        self.contexts.insert(name, auth);
    }

    /// The authenticator that bound context `name`, if any.
    #[must_use]
    pub fn bound_context_auth(&self, name: &str) -> Option<&str> {
        self.contexts.get(name).map(String::as_str)
    }

    /// The subscription named `sub`, if live.
    #[must_use]
    pub fn sub(&self, sub: &Sub) -> Option<&SubState> {
        self.subs.get(sub)
    }

    /// The subscription named `sub` for mutation.
    pub fn sub_mut(&mut self, sub: &Sub) -> Option<&mut SubState> {
        self.subs.get_mut(sub)
    }

    /// Install (or replace) a subscription.
    pub fn insert_sub(&mut self, sub: Sub, state: SubState) {
        self.subs.insert(sub, state);
    }

    /// The ids of every live subscription, for a connection-wide sweep.
    #[must_use]
    pub fn sub_ids(&self) -> Vec<Sub> {
        self.subs.keys().cloned().collect()
    }

    /// Record the §12.3 scope key an operation capability reads.
    pub fn record_operation(&mut self, id: liasse_wire::OperationId, key: liasse_surface::OperationKey) {
        self.operations.insert(id, key);
    }

    /// The §12.3 scope key an operation capability names, if this connection issued it.
    #[must_use]
    pub fn operation_key(&self, id: &liasse_wire::OperationId) -> Option<&liasse_surface::OperationKey> {
        self.operations.get(id)
    }

    /// Resolve an inbound occurrence token to the row it names, checking the public
    /// id first (a forged token never reaches the index). `Ok(None)` is a well-formed
    /// token for an occurrence this connection does not currently hold — an absent
    /// anchor, not a fault.
    #[must_use]
    pub fn resolve_occ(&self, minter: &dyn TokenMinter, occ: &Occ) -> AnchorResolution {
        if self.keys.open_occurrence(minter, occ.as_str()).is_none() {
            return AnchorResolution::Forged;
        }
        match self.occ_index.get(occ) {
            Some((_, row)) => AnchorResolution::Row(row.clone()),
            None => AnchorResolution::Absent,
        }
    }

    /// Mint (or reuse) the occurrence token for `row` within subscription `sub`.
    /// Stable within the subscription's life; the counter only grows, so a token is
    /// never reused for a different row.
    pub fn mint_occ(
        &mut self,
        minter: &dyn TokenMinter,
        sub: &Sub,
        row: &RowId,
    ) -> Occ {
        if let Some(state) = self.subs.get(sub)
            && let Some(occ) = state.occ_of.get(row)
        {
            return occ.clone();
        }
        let counter = self.occ_counter;
        self.occ_counter += 1;
        let occ = self.keys.occurrence(minter, counter);
        self.occ_index.insert(occ.clone(), (sub.clone(), row.clone()));
        if let Some(state) = self.subs.get_mut(sub) {
            state.occ_of.insert(row.clone(), occ.clone());
        }
        occ
    }
}

/// How an inbound occurrence token resolved.
pub enum AnchorResolution {
    /// The token names this row (§12.2 anchor present).
    Row(RowId),
    /// A well-formed token for an occurrence this connection does not hold — the
    /// window fails to open (absent anchor), not a fault.
    Absent,
    /// The token did not carry this connection's public id — a fault.
    Forged,
}

/// One subscription's client-facing tracking state.
pub struct SubState {
    /// Whether the subscription is a bounded window (§12.2).
    pub windowed: bool,
    /// Whether the subscription delivers a scalar/aggregate value (§7.5).
    pub scalar: bool,
    /// Whether the server has closed the subscription.
    pub closed: bool,
    /// The RowId→occurrence bijection (stable for the subscription's life).
    occ_of: BTreeMap<RowId, Occ>,
    /// The wire rows the client currently holds — the D6 diff baseline.
    pub snapshot: Vec<WireRow>,
    /// The scalar value the client currently holds, for a scalar subscription.
    pub scalar_value: Option<liasse_wire::serde_json::Value>,
}

impl SubState {
    /// A fresh row-stream subscription's tracking state.
    #[must_use]
    pub fn rows(windowed: bool) -> Self {
        Self {
            windowed,
            scalar: false,
            closed: false,
            occ_of: BTreeMap::new(),
            snapshot: Vec::new(),
            scalar_value: None,
        }
    }

    /// A fresh scalar subscription's tracking state.
    #[must_use]
    pub fn scalar() -> Self {
        Self {
            windowed: false,
            scalar: true,
            closed: false,
            occ_of: BTreeMap::new(),
            snapshot: Vec::new(),
            scalar_value: None,
        }
    }
}

/// One frame the ring retains, with the position that orders it and the frontier it
/// was stamped at (the resume cursor).
#[derive(Clone)]
pub struct Emitted {
    /// The frame's SSE `id:` — its frontier token.
    pub id: Ft,
    /// The numeric frontier behind that token, for replay-range decisions.
    pub frontier_seq: u64,
    /// The downstream frame.
    pub frame: Downstream,
    /// The per-connection monotone position that orders emissions.
    pos: u64,
}

/// A bounded ring of emitted frames: the live-delivery cursor, the retained history
/// for `Last-Event-ID` replay, and the backpressure latch (D3).
pub struct Outbound {
    ring: VecDeque<Emitted>,
    capacity: usize,
    next_pos: u64,
    cursor: u64,
    /// The highest frontier that has had a frame evicted: a resume from at or after
    /// it is still fully retained; older is a released range needing a fresh init.
    evicted_through: u64,
    /// An undelivered frame was dropped — the stream must reset and re-init (D3).
    overflow: bool,
}

impl Outbound {
    fn new(capacity: usize) -> Self {
        Self {
            ring: VecDeque::new(),
            capacity: capacity.max(1),
            next_pos: 0,
            cursor: 0,
            evicted_through: 0,
            overflow: false,
        }
    }

    /// Enqueue a frame at `frontier_seq` under SSE id `id`. Evicting an as-yet
    /// undelivered frame trips the overflow latch (D3): the actor never blocks, and
    /// the dropped stream is reconstructed on reconnect.
    pub fn enqueue(&mut self, id: Ft, frontier_seq: u64, frame: Downstream) {
        let pos = self.next_pos;
        self.next_pos += 1;
        self.ring.push_back(Emitted { id, frontier_seq, frame, pos });
        while self.ring.len() > self.capacity {
            if let Some(dropped) = self.ring.pop_front() {
                self.evicted_through = self.evicted_through.max(dropped.frontier_seq);
                if dropped.pos >= self.cursor {
                    self.overflow = true;
                }
            }
        }
    }

    /// Drain every frame the live writer has not yet been handed (§12.2 stream).
    pub fn drain_pending(&mut self) -> Vec<Emitted> {
        let out: Vec<Emitted> =
            self.ring.iter().filter(|e| e.pos >= self.cursor).cloned().collect();
        self.cursor = self.next_pos;
        out
    }

    /// Whether a resume from frontier `seq` is still fully retained (every frame
    /// after it survives in the ring).
    #[must_use]
    pub fn replayable(&self, seq: u64) -> bool {
        seq >= self.evicted_through
    }

    /// Replay every retained frame strictly after frontier `seq`, marking them
    /// delivered.
    pub fn replay_after(&mut self, seq: u64) -> Vec<Emitted> {
        let out: Vec<Emitted> =
            self.ring.iter().filter(|e| e.frontier_seq > seq).cloned().collect();
        self.cursor = self.next_pos;
        out
    }

    /// Take and clear the overflow latch.
    pub fn take_overflow(&mut self) -> bool {
        std::mem::replace(&mut self.overflow, false)
    }

    /// Mark everything currently buffered as delivered (after a fresh init supplants
    /// the retained stream).
    pub fn mark_delivered(&mut self) {
        self.cursor = self.next_pos;
    }
}
