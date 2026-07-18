//! Serializing the upstream requests the shell POSTs, and splitting an SSE text
//! stream — the marshaling the client needs WITHOUT holding any authority.
//!
//! The wasm core never fetches and never mints a token: it only turns the shell's
//! typed request into the canonical wire body (through the shared [`liasse_wire`]
//! frame types, so the server decodes exactly what this crate produced) and hands
//! back the SSE lines a raw stream reader would otherwise parse in JS. The
//! connection token header, the §12.3 operation-id header, and the actual transport
//! are the shell's concern.

use serde::Serialize;

use liasse_wire::{OperationId, SseEvent, Sub, Upstream, Value, WireWindow, encode};

use crate::error::CoreError;

/// Encode a `hello` (§11): open the connection, optionally with an authentication
/// selection and a §11.8 context. Both are opaque values the server decodes.
///
/// # Errors
/// [`CoreError::Codec`] if the request cannot be rendered.
pub fn hello(auth: Option<Value>, context: Option<Value>) -> Result<String, CoreError> {
    Ok(encode(&Upstream::Hello { auth, context })?)
}

/// Encode a `manifest` request (§12.1).
///
/// # Errors
/// [`CoreError::Codec`] if the request cannot be rendered.
pub fn manifest() -> Result<String, CoreError> {
    Ok(encode(&Upstream::Manifest)?)
}

/// Encode a `view` (§12.2): open or replace subscription `sub` over `address`, with
/// optional parameters, a bounded window, an authentication selection, and a
/// context. The engine-interpreted members are opaque values decoded server-side.
///
/// # Errors
/// [`CoreError::Codec`] if the request cannot be rendered.
pub fn view(
    sub: &str,
    address: &str,
    params: Option<Value>,
    window: Option<WireWindow>,
    auth: Option<Value>,
    context: Option<Value>,
) -> Result<String, CoreError> {
    Ok(encode(&Upstream::View {
        sub: Sub::new(sub),
        address: address.to_owned(),
        params,
        window,
        auth,
        context,
    })?)
}

/// Encode an `unsubscribe` (§12.2): end subscription `sub`.
///
/// # Errors
/// [`CoreError::Codec`] if the request cannot be rendered.
pub fn unsubscribe(sub: &str) -> Result<String, CoreError> {
    Ok(encode(&Upstream::Unsubscribe { sub: Sub::new(sub) })?)
}

/// Encode a `call` (§10, §12.3) over `address` with `args`, optionally carrying an
/// authentication selection and a context. The §12.3 operation capability is a
/// transport header, not part of this body.
///
/// # Errors
/// [`CoreError::Codec`] if the request cannot be rendered.
pub fn call(
    address: &str,
    args: Value,
    auth: Option<Value>,
    context: Option<Value>,
) -> Result<String, CoreError> {
    Ok(encode(&Upstream::Call { address: address.to_owned(), args, auth, context })?)
}

/// Encode a `fetch` (§12.1): a one-shot read of `address` at the current frontier.
///
/// # Errors
/// [`CoreError::Codec`] if the request cannot be rendered.
pub fn fetch(address: &str, params: Option<Value>) -> Result<String, CoreError> {
    Ok(encode(&Upstream::Fetch { address: address.to_owned(), params })?)
}

/// Encode an `operation` status query (§12.3) for the operation capability `id`.
///
/// # Errors
/// [`CoreError::Codec`] if the request cannot be rendered.
pub fn operation(id: &str) -> Result<String, CoreError> {
    Ok(encode(&Upstream::Operation { operation: OperationId::new(id) })?)
}

/// One parsed SSE event: the fields the shell routes on. `id` is the frontier token
/// to pass alongside the frame to [`crate::ClientReplica::apply`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SseLine {
    /// The `event:` type, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// The `id:` value — the frontier token for this frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The `data:` payload — the downstream frame JSON to hand to `apply`.
    pub data: String,
    /// The `retry:` reconnection delay in milliseconds, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<u64>,
}

impl From<SseEvent> for SseLine {
    fn from(event: SseEvent) -> Self {
        Self { event: event.event, id: event.id, data: event.data, retry: event.retry }
    }
}

/// Split an SSE text stream into its dispatched events (§12.2 downstream framing).
/// A shell reading a raw byte stream uses this to recover `(event, id, data)`; a
/// shell on a native `EventSource` already has them and skips it. Total and
/// panic-free: arbitrary text yields some (possibly empty) set of events.
#[must_use]
pub fn parse_sse(text: &str) -> Vec<SseLine> {
    SseEvent::parse_stream(text).into_iter().map(SseLine::from).collect()
}
