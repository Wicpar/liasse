//! The WASM client core of the Liasse client-sync connector (§12): a wasm-bindgen
//! shell over [`liasse_wire`] that applies already-authorized downstream frames and
//! serializes upstream request bodies, holding NO authority.
//!
//! # What it is
//! The untrusted web client is a convenience layer for view-state sync and request
//! ergonomics only (AGENTS.md). This crate is its core: it wraps
//! [`liasse_wire::WireStore`] per subscription in a connection-level
//! [`ClientReplica`], folds each decoded downstream frame (`init`/`scalar`/`patch`/
//! `close`/`frontier`/`reset`/`fault`) into the right replica through the shared
//! [`liasse_wire::apply`], and reports the client-visible effect. It also encodes the
//! upstream frames (`hello`/`manifest`/`view`/`unsubscribe`/`call`/`fetch`/
//! `operation`) the TS shell POSTs. All authorization, projection, and token minting
//! stay server-side; here every token is inert data the client only echoes.
//!
//! # One source of truth
//! The §12.2 apply semantics and the wire schema live in `liasse-wire`. This crate
//! reconstructs nothing: it tracks rows by opaque occurrence token and renders the
//! opaque projected [`liasse_wire::Value`] verbatim, never rebuilding a typed engine
//! value. The wasm-bindgen boundary ([`wasm`], compiled only for `wasm32`) merely
//! marshals between `JsValue` and this core.
//!
//! # Trust boundary
//! Every inbound frame is parsed as hostile input: decoding is total and the core
//! never panics (AGENTS.md). A malformed frame, a patch for an unopened subscription,
//! or a patch that does not apply returns a [`CoreError`]; at the wasm boundary that
//! becomes a `JsValue` error, never an abort.
//!
//! # Native vs wasm
//! The pure core ([`ClientReplica`], [`request`]) is engine-free safe Rust that builds
//! and tests on any target. The `wasm-bindgen` surface is `#[cfg(target_arch =
//! "wasm32")]`, so a native `cargo check`/`clippy` exercises the logic without the
//! wasm-only dependencies.

mod error;
mod replica;
pub mod request;

#[cfg(target_arch = "wasm32")]
mod wasm;

pub use error::CoreError;
pub use replica::{Applied, AppliedKind, ClientReplica, Fault};
pub use request::SseLine;

#[cfg(target_arch = "wasm32")]
pub use wasm::{
    OperationHandle, WireClient, encode_call, encode_fetch, encode_hello, encode_manifest,
    encode_operation, encode_unsubscribe, encode_view, parse_sse,
};

// Re-export the wire vocabulary the core speaks so a consumer builds frames and reads
// values without pinning its own `liasse-wire`/`serde_json`.
pub use liasse_wire::{self, Value, serde_json};
