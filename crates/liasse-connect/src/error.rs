//! The transport-fault taxonomy — distinct from a spec outcome.
//!
//! A [`ConnectError`] is a failure of the connection *mechanism*: an unknown or
//! forged capability token, a frame that did not parse, an oversized body, or a
//! host/store fault. It is never a §10/§11/§12 outcome — a denied, rejected, or
//! failed request is a *successful observation* of that outcome and travels in the
//! wire [`Outcome`](liasse_wire::Outcome), not here.
//!
//! Each variant maps to a stable wire [`FaultCode`] (a downstream `fault` frame, or
//! an HTTP 4xx/5xx in the reference binding). The message is always sanitized: it
//! names the fault class, never a credential, an internal identity, or another
//! actor's data (AGENTS.md).

use liasse_wire::FaultCode;

/// A transport-level fault the connection could not turn into a spec outcome.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// A request presented a connection capability the server does not recognize —
    /// it was forged, or it belongs to a connection that has since closed (a host
    /// restart drops volatile connections, §22).
    #[error("no open connection matches the presented capability")]
    NoConnection,
    /// A presented occurrence, frontier, or operation capability was forged,
    /// expired, or belongs to another connection (§12.2, §12.3).
    #[error("a presented capability token is not valid on this connection")]
    BadToken,
    /// An inbound frame did not parse as a well-formed request (§12.1). Carries the
    /// codec's own diagnostic, which describes only the wire shape, never state.
    #[error("malformed inbound frame: {0}")]
    Codec(#[from] liasse_wire::CodecError),
    /// An inbound body exceeded the connection's size bound before it was parsed.
    #[error("inbound frame exceeds the {limit}-byte bound")]
    Oversized {
        /// The byte bound the body exceeded.
        limit: usize,
    },
    /// A store or engine fault surfaced from the host while serving the request. It
    /// is not a refusal (which is an outcome) but a broken mechanism.
    #[error("host fault: {0}")]
    Host(#[from] liasse_surface::SurfaceError),
}

impl ConnectError {
    /// The stable wire fault class this error is reported as.
    #[must_use]
    pub fn code(&self) -> FaultCode {
        match self {
            Self::NoConnection | Self::BadToken => FaultCode::BadToken,
            Self::Codec(_) => FaultCode::Malformed,
            Self::Oversized { .. } => FaultCode::Oversized,
            Self::Host(_) => FaultCode::Internal,
        }
    }

    /// A sanitized, class-level message safe to place in a `fault` frame. It never
    /// echoes the host's internal diagnostic (which could name state), unlike the
    /// [`Display`](std::fmt::Display) form used for local logging.
    #[must_use]
    pub fn sanitized(&self) -> String {
        match self.code() {
            FaultCode::BadToken => "capability token not recognized".to_owned(),
            FaultCode::Malformed => "frame did not parse".to_owned(),
            FaultCode::Oversized => "frame exceeds the size bound".to_owned(),
            FaultCode::Internal => "internal error".to_owned(),
        }
    }
}
