//! The ephemeral per-socket stream-session registry — the §12.2 anti-theft binding.
//!
//! The downstream SSE stream is opened ANONYMOUSLY: no connection credential, no
//! cookie, no token in the URL. On connect the server mints a fresh high-entropy
//! [`StreamSession`] and announces it as the socket's FIRST event; the socket is
//! registered here UNBOUND. A subscription's frames flow to that socket only after an
//! authenticated `view` POST presents both the connection credential `C` (§11/§12.2)
//! and the session id, at which point the session is BOUND to `C` — and thereafter only
//! `C` may attach subscriptions to it.
//!
//! This is what makes a stream unstealable. The session id is delivered ONLY in-band on
//! the victim's own socket's first event (never a URL, cookie, or log), so:
//!
//! - a stolen session id is worthless without `C` — attaching still requires the
//!   authenticated bind, and the id grants no authority on its own; and
//! - a stolen `C` cannot attach to the victim's socket — the victim's session id was
//!   never transmitted anywhere the thief could read, and a `C` that presents a
//!   *different* connection's already-bound session is rejected.
//!
//! Opening the anonymous URL therefore yields only a fresh empty unbound session; data
//! flows only after an authenticated bind.

use std::collections::BTreeMap;

use liasse_wire::ConnectionToken;

/// The SSE `event:` type carrying the stream-session announcement (the socket's first
/// event), kept DISTINCT from the default `message` events that carry §12.2 wire frames
/// so the frozen wasm core only ever sees frames.
pub const STREAM_SESSION_EVENT: &str = "liasse-session";

/// An ephemeral per-socket stream-session id: the handle an anonymous SSE socket is
/// known by. High-entropy and opaque; it is NOT a bearer credential — it cannot open or
/// attach to a stream on its own, because an authenticated bind still requires `C`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct StreamSession(String);

impl StreamSession {
    /// Wrap a minted session id (a [`TokenMinter`](crate::token::TokenMinter) nonce) or a
    /// client-presented `Liasse-Stream` header value.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id text: the `{"stream": ...}` announcement value and the `Liasse-Stream`
    /// header the bind POST echoes back.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a bind attempt resolved.
pub enum StreamBind {
    /// The session was bound to the connection — freshly, or it already belonged to it
    /// (idempotent: the same `C` re-attaching, e.g. a second subscription on one socket).
    Bound,
    /// The session id names no live socket — a fault.
    Unknown,
    /// The session is already bound to a DIFFERENT connection — theft, a fault.
    Stolen,
}

/// The live anonymous sockets, each mapped to the connection it is bound to (`None`
/// while still unbound). One socket belongs to at most one connection; the reverse — a
/// connection's current delivery socket — is held on the connection state.
#[derive(Default)]
pub struct Streams {
    bindings: BTreeMap<StreamSession, Option<ConnectionToken>>,
}

impl Streams {
    /// Register a fresh unbound socket under `id`.
    pub fn open(&mut self, id: String) -> StreamSession {
        let session = StreamSession::new(id);
        self.bindings.insert(session.clone(), None);
        session
    }

    /// Bind `session` to `conn` on first authenticated use (§12.2). Idempotent for the
    /// SAME connection; a DIFFERENT connection presenting an already-bound session is
    /// [`StreamBind::Stolen`]; a session that names no live socket is
    /// [`StreamBind::Unknown`]. Never panics on a forged or stale id.
    pub fn bind(&mut self, session: &StreamSession, conn: &ConnectionToken) -> StreamBind {
        match self.bindings.get_mut(session) {
            None => StreamBind::Unknown,
            Some(slot) => match slot.as_ref() {
                Some(owner) if owner == conn => StreamBind::Bound,
                Some(_) => StreamBind::Stolen,
                None => {
                    *slot = Some(conn.clone());
                    StreamBind::Bound
                }
            },
        }
    }

    /// The connection a session is bound to, if any.
    #[must_use]
    pub fn bound(&self, session: &StreamSession) -> Option<&ConnectionToken> {
        self.bindings.get(session).and_then(Option::as_ref)
    }

    /// Forget a dead socket (its writer ended).
    pub fn close(&mut self, session: &StreamSession) {
        self.bindings.remove(session);
    }
}
