//! The adapter's transport/host error type.
//!
//! A [`Driver::Error`](crate::Driver) is reserved for a harness fault — never a
//! spec outcome. Two things surface as one here: a genuine host fault (a store
//! error, a malformed address the harness cannot form a request from), and a
//! step whose [`OpRequest`](crate::OpRequest) kind this phase does not yet drive.
//! The engine turns either into a `Skipped` step with the message as its reason,
//! which is exactly the behaviour this phase wants for an unimplemented kind:
//! recorded as skipped, never a panic.

use std::fmt;

/// A harness-level failure the driver reports instead of an [`Observation`].
///
/// [`Observation`]: crate::Observation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    /// The case's package did not load, so no step can run against it.
    LoadFailed(String),
    /// The step's action is outside the set this phase drives; recorded as a
    /// skip so the triage loop can harden it later.
    Unsupported(String),
    /// A host/transport fault surfaced from the surface layer or the store.
    Host(String),
}

impl AdapterError {
    /// An unsupported-kind skip carrying `reason`.
    #[must_use]
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self::Unsupported(reason.into())
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoadFailed(message) => write!(f, "package did not load: {message}"),
            Self::Unsupported(reason) => write!(f, "unsupported step: {reason}"),
            Self::Host(message) => write!(f, "host fault: {message}"),
        }
    }
}

impl std::error::Error for AdapterError {}
