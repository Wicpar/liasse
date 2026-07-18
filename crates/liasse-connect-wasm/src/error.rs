//! Why a client operation over the wire could not complete.
//!
//! The core never panics on inbound data (AGENTS.md): a malformed downstream frame,
//! a patch that does not describe a valid transition, or a patch for a subscription
//! the client never opened all surface as a [`CoreError`]. The wasm-bindgen boundary
//! maps each into a `JsValue` for the TS shell; the pure core returns it as an
//! ordinary `Result`.

use liasse_wire::{CodecError, StoreError};

/// A failure applying a downstream frame or encoding an upstream request.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The frame (or a value inside an upstream request) was not well-formed for its
    /// target type — malformed JSON, a wrong type, a missing field, an unknown tag.
    #[error(transparent)]
    Codec(#[from] CodecError),
    /// A well-formed frame did not fit the addressed subscription's state (a patch
    /// before `init`, a shape change, a frame after close/reset, or a patch that does
    /// not apply to the current result).
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A `patch` (or `close`-less) frame named a subscription the client is not
    /// tracking. `init`/`scalar` open a subscription; a `patch` for an unknown one is
    /// a server/client disagreement the client refuses rather than inventing state.
    #[error("frame targets subscription `{0}` which is not open on this connection")]
    NotSubscribed(String),
}
