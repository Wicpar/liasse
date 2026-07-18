//! Server-to-client frames — the SSE stream of one logical connection (§12.2).
//!
//! Each per-subscription data frame (`init`/`scalar`/`patch`/`close`) carries its
//! [`Sub`] so the client routes it to the right live view. The connection-level
//! frames (`frontier`/`reset`/`fault`) carry none: they concern the whole stream.
//! The frontier token that stamps each advance is the SSE `id:`, not a body field,
//! which is why [`Downstream::Frontier`] has no payload.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::patch::PatchOp;
use crate::row::WireRow;
use crate::token::Sub;

/// A frame sent from server to client over the connection's SSE stream. Tagged by
/// `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Downstream {
    /// The complete initial row set of a row-stream subscription (§12.2 `init`).
    Init {
        /// The subscription this row set opens.
        sub: Sub,
        /// The rows at the opening frontier, in view order.
        rows: Vec<WireRow>,
    },
    /// A scalar/aggregate subscription's value (§7.5, §12.2). The value at first
    /// observation or when it changed; a frontier-only no-op is a bare `frontier`.
    Scalar {
        /// The subscription this value belongs to.
        sub: Sub,
        /// The scalar value, rendered by the engine.
        value: Value,
    },
    /// An ordered §12.2 patch advancing a row-stream subscription. An empty `ops`
    /// is the frontier-only patch (nothing changed at this frontier).
    Patch {
        /// The subscription this patch advances.
        sub: Sub,
        /// The operations, applied in listed order (see [`crate::apply`]).
        ops: Vec<PatchOp>,
    },
    /// A subscription the server ended (§12.2). No further frames carry this `sub`.
    Close {
        /// The subscription being closed.
        sub: Sub,
        /// Why it closed.
        reason: CloseReason,
    },
    /// A frontier-only advance of the whole connection: nothing changed for any
    /// subscription, but the connection frontier moved. The new frontier token is
    /// the SSE `id:` on this frame, so the body is empty.
    Frontier,
    /// The connection is out of sync and the client must re-establish its
    /// subscriptions from scratch — its retained frontier is no longer replayable.
    Reset {
        /// Why the connection was reset.
        reason: ResetReason,
    },
    /// A transport-level fault distinct from a spec outcome: a malformed, oversized,
    /// or forged input the connection could not process. It never carries model
    /// state, only a stable code and a sanitized message.
    Fault {
        /// A stable machine-readable fault class.
        code: FaultCode,
        /// A sanitized human-readable description.
        message: String,
    },
}

/// Why a subscription closed (§12.2). Informational for the client, which stops
/// tracking the subscription regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloseReason {
    /// A per-frontier authorization re-check failed: the actor is no longer
    /// permitted the view (§11), so the stream stops rather than leak the change.
    Unauthorized,
    /// The client asked to end this subscription (`unsubscribe`).
    Unsubscribed,
    /// A new subscription replaced this one on the same `sub`.
    Replaced,
    /// The server closed the subscription for a reason not otherwise distinguished.
    ServerClosed,
}

/// Why the whole connection was reset (§12.2). In every case the client re-views
/// from the current frontier; no retained patch stream is replayable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResetReason {
    /// The server does not recognize the connection — it restarted, or the
    /// connection's volatile subscriptions (§22) did not survive.
    UnknownConnection,
    /// The connection's outbound buffer overflowed and frames were dropped; the
    /// client must re-init rather than resume from a gap (lossless by
    /// reconstruction).
    Overflow,
    /// The server reset the connection for a reason not otherwise distinguished.
    ServerReset,
}

/// A stable transport-fault class (distinct from a spec [`crate::Outcome`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaultCode {
    /// A presented capability token (occurrence, frontier, connection, operation)
    /// was forged, expired, or belongs to another connection.
    BadToken,
    /// An inbound frame did not parse as a well-formed request.
    Malformed,
    /// An inbound frame exceeded the connection's size bound.
    Oversized,
    /// The server hit an internal error it will not describe further.
    Internal,
}
