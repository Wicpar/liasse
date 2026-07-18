//! The reply a request handler returns before the binding serializes it.
//!
//! A downstream `init`/`patch`/`scalar`/`close` never travels in a reply — it is
//! enqueued on the connection's SSE stream (§12.2), and the reply only acknowledges
//! the request or carries its §10/§11/§12 [`Outcome`]. Success and refusal are both
//! ordinary replies; a transport fault is the separate `Err(ConnectError)` the
//! binding turns into a `fault` frame.

use liasse_wire::serde_json::Value as Json;
use liasse_wire::{ConnectionToken, Ft, Outcome};

/// The result of a client request (§12.1), distinct from the SSE stream it may have
/// enqueued frames onto.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// A connection opened; the client presents `connection` on every later request.
    Hello {
        /// The connection capability.
        connection: ConnectionToken,
    },
    /// The surfaces exposed to the connection's context (§12.1 `manifest`).
    Manifest(Vec<String>),
    /// A subscription opened; its `init`/`scalar` frame is on the SSE stream at
    /// `frontier`.
    Opened {
        /// The frontier the initial result was delivered at.
        frontier: Ft,
    },
    /// A subscription ended at the client's request.
    Unsubscribed,
    /// A snapshot read's projected value (§12.1 `fetch`): an array of row objects, or
    /// a scalar.
    Fetched(Json),
    /// A call/fetch/operation outcome, or a subscription refusal (§10/§11/§12).
    Outcome(Outcome),
}
