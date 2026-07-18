//! [`ConnectCore`]: the transport-agnostic sync heart of the connector (§12).
//!
//! One owner drives it single-threaded, one request at a time — a plain `&mut self`
//! object over the [`SurfaceHost`], with no interior mutability and no async. Every
//! external effect flows through the host's public admission and view API; this
//! layer adds only framing, opaque identity, and the §12.2 stream. A reference
//! binding (`bind::std_http`) wraps it in an actor thread, but the loopback
//! conformance suite drives it directly, in process.

mod command;
mod frames;
mod live;
mod registry;
mod sweep;

pub use command::Reply;

use std::collections::BTreeMap;

use liasse_store::InstanceStore;
use liasse_surface::{Authenticate, CommitSeq, SurfaceHost};
use liasse_wire::{
    CloseReason, ConnectionToken, Downstream, Ft, ResetReason, SseEvent, Upstream,
};

use crate::decode;
use crate::error::ConnectError;
use crate::mount::Schema;
use crate::token::{ConnKeys, TokenMinter, UnsignedMinter};

use registry::{ConnState, Emitted};

/// The default per-connection outbound bound: frames buffered before backpressure
/// drops the SSE stream and reconstructs it on reconnect (D3).
const DEFAULT_CAPACITY: usize = 256;

/// The connector core: the surface host, the decode contract, the token minter, and
/// the live connection registry, owned by value.
pub struct ConnectCore<S> {
    host: SurfaceHost<S>,
    schema: Schema,
    minter: Box<dyn TokenMinter>,
    connections: BTreeMap<ConnectionToken, ConnState>,
    capacity: usize,
}

impl<S: InstanceStore> ConnectCore<S> {
    /// Mount `host` behind `schema`, minting tokens with the default unsigned minter.
    #[must_use]
    pub fn mount(host: SurfaceHost<S>, schema: Schema) -> Self {
        Self::with_minter(host, schema, Box::new(UnsignedMinter::new()))
    }

    /// Mount with a specific [`TokenMinter`] (the D4 seam: HMAC, signed, …).
    #[must_use]
    pub fn with_minter(host: SurfaceHost<S>, schema: Schema, minter: Box<dyn TokenMinter>) -> Self {
        Self { host, schema, minter, connections: BTreeMap::new(), capacity: DEFAULT_CAPACITY }
    }

    /// Set the per-connection outbound bound (defaults to [`DEFAULT_CAPACITY`]).
    #[must_use]
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    /// The underlying host, for reading committed state directly (tests, operators).
    #[must_use]
    pub fn host(&self) -> &SurfaceHost<S> {
        &self.host
    }

    /// The frontier position an [`Ft`] names on `conn`, or `None` if it is forged or
    /// belongs to another connection. Lets a caller check the §12 monotonicity of an
    /// SSE `id:` stream without trusting the token's text.
    #[must_use]
    pub fn frontier_position(&self, conn: &ConnectionToken, ft: &Ft) -> Option<u64> {
        let state = self.connections.get(conn)?;
        state.keys().open_frontier(self.minter.as_ref(), ft.as_str())
    }

    /// Dispatch one decoded inbound frame (§12.1). `operation` is the §12.3 capability
    /// a `call` carries as transport metadata; other frames ignore it. A refusal is a
    /// [`Reply`], a broken mechanism an `Err`.
    pub fn submit(
        &mut self,
        conn: Option<&ConnectionToken>,
        operation: Option<liasse_wire::OperationId>,
        frame: Upstream,
    ) -> Result<Reply, ConnectError> {
        match frame {
            Upstream::Hello { auth, context } => self.hello(auth.as_ref(), context.as_ref()),
            Upstream::Manifest => self.manifest(self.require(conn)?),
            Upstream::View { sub, address, params, window, auth, context } => {
                let token = self.require(conn)?.clone();
                self.view(&token, sub, &address, params.as_ref(), window, auth.as_ref(), context.as_ref())
            }
            Upstream::Unsubscribe { sub } => {
                let token = self.require(conn)?.clone();
                self.unsubscribe(&token, &sub)
            }
            Upstream::Call { address, args, auth, context } => {
                let token = self.require(conn)?.clone();
                self.call(&token, operation, &address, &args, auth.as_ref(), context.as_ref())
            }
            Upstream::Fetch { address, params } => {
                let token = self.require(conn)?.clone();
                self.fetch(&token, &address, params.as_ref())
            }
            Upstream::Operation { operation } => self.operation(self.require(conn)?, &operation),
        }
    }

    /// The open connection a request must name (§12.1). A missing, forged, or closed
    /// connection capability is [`ConnectError::NoConnection`] — checked here, before
    /// the frame reaches the host, so an unknown token is a fault, never an internal
    /// error.
    fn require<'t>(&self, conn: Option<&'t ConnectionToken>) -> Result<&'t ConnectionToken, ConnectError> {
        let token = conn.ok_or(ConnectError::NoConnection)?;
        if self.connections.contains_key(token) {
            Ok(token)
        } else {
            Err(ConnectError::NoConnection)
        }
    }

    /// Open a connection (§12), authenticating its default context when `hello`
    /// carries a selection (§11.4). A credential the schema cannot decode, or a
    /// denied selection, still opens the connection unauthenticated — the client may
    /// re-authenticate per request — so `hello` never leaks why a credential failed.
    fn hello(
        &mut self,
        auth: Option<&liasse_wire::serde_json::Value>,
        context: Option<&liasse_wire::serde_json::Value>,
    ) -> Result<Reply, ConnectError> {
        // Two independent high-entropy draws: the secret credential `C` (the token /
        // cookie / registry key) and the public id `P` embedded in this connection's
        // ft/occ. `C` never enters a token, so a leaked ft/occ never yields it.
        let keys = ConnKeys::new(self.minter.nonce(), self.minter.nonce());
        let token = keys.connection_token();
        self.host.connect(token.as_str());
        self.connections.insert(token.clone(), ConnState::new(keys, self.capacity));

        if let Some(auth) = auth
            && let Ok(hello) = decode::decode_hello_auth(&self.schema, auth)
        {
            let context_name = decode::decode_context(context).ok().flatten();
            let mut request = Authenticate::new(hello.role, hello.selection);
            if let Some(name) = context_name {
                request = request.as_context(name);
            }
            if let Ok(liasse_surface::AuthResult::Bound) = self.host.authenticate(token.as_str(), &request) {
                self.record_context(&token, request.context(), request.selection().auth());
            }
        }
        Ok(Reply::Hello { connection: token })
    }

    /// The surfaces exposed to the connection's context (§12.1 `manifest`).
    fn manifest(&self, conn: &ConnectionToken) -> Result<Reply, ConnectError> {
        if !self.connections.contains_key(conn) {
            return Err(ConnectError::NoConnection);
        }
        let surfaces = self.host.manifest(conn.as_str(), None)?;
        Ok(Reply::Manifest(surfaces))
    }

    /// Query a retained §12.3 operation status. An id this connection never issued —
    /// forged or foreign — reads as `unknown`, never leaking another client's record.
    fn operation(
        &self,
        conn: &ConnectionToken,
        operation: &liasse_wire::OperationId,
    ) -> Result<Reply, ConnectError> {
        let state = self.connections.get(conn).ok_or(ConnectError::NoConnection)?;
        let outcome = match state.operation_key(operation) {
            Some(key) => {
                let status = self.host.operation_status(key);
                crate::encode::status_outcome(&status, |seq| state.keys().frontier(self.minter.as_ref(), seq.get()))
            }
            None => liasse_wire::Outcome::Unknown,
        };
        Ok(Reply::Outcome(outcome))
    }

    /// End a subscription at the client's request (§12.2): mark it closed, release
    /// its tracking state, and emit a terminal `close`.
    fn unsubscribe(&mut self, conn: &ConnectionToken, sub: &liasse_wire::Sub) -> Result<Reply, ConnectError> {
        let seq = self.frontier_seq(conn);
        let state = self.connections.get_mut(conn).ok_or(ConnectError::NoConnection)?;
        let live = state.sub(sub).is_some_and(|s| !s.closed);
        if live {
            if let Some(sub_state) = state.sub_mut(sub) {
                sub_state.closed = true;
            }
            let ft = state.keys().frontier(self.minter.as_ref(), seq);
            state.outbound_mut().enqueue(
                ft,
                seq,
                Downstream::Close { sub: sub.clone(), reason: CloseReason::Unsubscribed },
            );
        }
        Ok(Reply::Unsubscribed)
    }

    /// Close a connection, dropping its subscriptions and volatile state (§22).
    pub fn disconnect(&mut self, conn: &ConnectionToken) {
        self.host.disconnect(conn.as_str());
        self.connections.remove(conn);
    }

    /// Drain the connection's SSE stream for the live writer (§12.2). A backpressure
    /// overflow (D3) supersedes the buffered stream with a `reset` + fresh init.
    pub fn poll(&mut self, conn: &ConnectionToken) -> Result<Vec<SseEvent>, ConnectError> {
        if !self.connections.contains_key(conn) {
            return Err(ConnectError::NoConnection);
        }
        if self.take_overflow(conn) {
            return Ok(self.reinit(conn, Some(ResetReason::Overflow)));
        }
        let emitted = self
            .connections
            .get_mut(conn)
            .map(|state| state.outbound_mut().drain_pending())
            .unwrap_or_default();
        Ok(emitted.iter().map(sse_event).collect())
    }

    /// (Re)connect the SSE stream (§12.2 resume). `Last-Event-ID` replays the retained
    /// tail when it is still buffered; otherwise — a released range, a forged id, an
    /// overflow, or a bare connect — the connection is reconstructed with a fresh init
    /// at the current frontier (`SurfaceHost::resume` semantics; always correct).
    pub fn resume(
        &mut self,
        conn: &ConnectionToken,
        last_event_id: Option<&str>,
    ) -> Result<Vec<SseEvent>, ConnectError> {
        if !self.connections.contains_key(conn) {
            // §22: the host does not recognize the connection — restarted, or its
            // volatile subscriptions did not survive. The client re-establishes.
            let event = reset_event(ResetReason::UnknownConnection);
            return Ok(vec![event]);
        }
        if self.take_overflow(conn) {
            return Ok(self.reinit(conn, Some(ResetReason::Overflow)));
        }
        if let Some(id) = last_event_id {
            if let Some(seq) = self.frontier_position(conn, &Ft::new(id))
                && self.connections.get(conn).is_some_and(|s| s.outbound_replayable(seq))
            {
                let emitted = self
                    .connections
                    .get_mut(conn)
                    .map(|state| state.outbound_mut().replay_after(seq))
                    .unwrap_or_default();
                return Ok(emitted.iter().map(sse_event).collect());
            }
            // A released range (or forged id): the retained tail cannot reproduce the
            // client's state, so re-init from scratch (D3: lossless by reconstruction).
            return Ok(self.reinit(conn, Some(ResetReason::ServerReset)));
        }
        // A bare connect with no resume point: fresh init, no reset needed.
        Ok(self.reinit(conn, None))
    }

    /// Take and clear the overflow latch of `conn`.
    fn take_overflow(&mut self, conn: &ConnectionToken) -> bool {
        self.connections
            .get_mut(conn)
            .is_some_and(|state| state.outbound_mut().take_overflow())
    }

    /// The current frontier position of `conn` (0 before any commit).
    fn frontier_seq(&self, conn: &ConnectionToken) -> u64 {
        self.host.frontier(conn.as_str()).map_or(0, CommitSeq::get)
    }

    /// Record that context `name` on `conn` was bound under authenticator `auth`, so
    /// a later §12.3 status query can reconstruct the operation scope key.
    fn record_context(&mut self, conn: &ConnectionToken, name: &str, auth: &str) {
        if let Some(state) = self.connections.get_mut(conn) {
            state.record_bound_context(name.to_owned(), auth.to_owned());
        }
    }

    /// Reconstruct the connection's SSE stream: an optional `reset`, then a fresh
    /// `init`/`scalar` per live subscription at the current frontier. The retained
    /// stream is superseded, so a subsequent `poll` re-sends nothing.
    fn reinit(&mut self, conn: &ConnectionToken, reset: Option<ResetReason>) -> Vec<SseEvent> {
        let seq = self.frontier_seq(conn);
        if let Some(state) = self.connections.get_mut(conn) {
            state.outbound_mut().mark_delivered();
            let ft = state.keys().frontier(self.minter.as_ref(), seq);
            if let Some(reason) = reset {
                state.outbound_mut().enqueue(ft, seq, Downstream::Reset { reason });
            }
        }
        let sub_ids = self.connections.get(conn).map(ConnState::sub_ids).unwrap_or_default();
        for sub in sub_ids {
            self.reproject_init(conn, &sub, seq);
        }
        self.connections
            .get_mut(conn)
            .map(|state| state.outbound_mut().drain_pending())
            .unwrap_or_default()
            .iter()
            .map(sse_event)
            .collect()
    }
}

/// The SSE event carrying one downstream frame: its frontier token as the `id:`, its
/// tag as the `event:`, and the JSON frame as `data:`.
fn sse_event(emitted: &Emitted) -> SseEvent {
    let data = liasse_wire::encode(&emitted.frame).unwrap_or_default();
    SseEvent::data(data).with_id(emitted.id.as_str()).with_event(frame_tag(&emitted.frame))
}

/// A connection-level `reset` event: it addresses the whole stream, so it carries no
/// per-subscription id.
fn reset_event(reason: ResetReason) -> SseEvent {
    let frame = Downstream::Reset { reason };
    let data = liasse_wire::encode(&frame).unwrap_or_default();
    SseEvent::data(data).with_event("reset")
}

/// The SSE `event:` tag naming a downstream frame's kind.
fn frame_tag(frame: &Downstream) -> &'static str {
    match frame {
        Downstream::Init { .. } => "init",
        Downstream::Scalar { .. } => "scalar",
        Downstream::Patch { .. } => "patch",
        Downstream::Close { .. } => "close",
        Downstream::Frontier => "frontier",
        Downstream::Reset { .. } => "reset",
        Downstream::Fault { .. } => "fault",
    }
}
