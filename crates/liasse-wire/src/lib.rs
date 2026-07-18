//! Engine-free wire schema, JSON codec, and §12.2 patch apply for the Liasse
//! client-sync connector.
//!
//! This crate is the single source of truth shared by three consumers: the server
//! codec in `liasse-connect`, the WASM client core in `liasse-connect-wasm`, and
//! the conformance corpus. Because all three agree on one set of message types and
//! one [`apply`] function, a client can reproduce the authorized §12.2 view without
//! any party re-deriving the semantics.
//!
//! # Engine-free by construction
//! The only dependencies are `serde`, `serde_json`, and `thiserror` — no
//! `liasse-*` crate, nothing that pulls the parser, the value model, or storage.
//! The crate builds for `wasm32-unknown-unknown`, which is the gate that keeps the
//! untrusted web client small and free of engine internals. Everything the engine
//! renders — a row value, a view parameter, a call argument, a response — crosses
//! this boundary as an opaque [`serde_json::Value`] that this crate carries but
//! never interprets.
//!
//! # What is on the wire
//! - Opaque capability tokens ([`Occ`], [`Ft`], [`Sub`], [`ConnectionToken`],
//!   [`OperationId`]) — never an internal `RowId`, `CommitSeq`, or session id.
//! - Rows ([`WireRow`]) and the ordered patch vocabulary ([`PatchOp`]), applied by
//!   the one [`apply`].
//! - Frames grouped by direction: [`Downstream`], [`Upstream`], and the request
//!   [`Outcome`].
//! - A per-subscription client replica ([`WireStore`]) that folds downstream frames
//!   into the current result.
//! - The [`codec`] (JSON, default) and the [`sse`] line framing that carries it.
//!
//! # Trust boundary
//! Every inbound frame is parsed as hostile input (AGENTS.md): decoding is total
//! and never panics, and no capability token is trusted here — this crate only
//! shapes and moves data, while authorization, projection, and token minting stay
//! server-side.

pub mod codec;
pub mod sse;

mod frame;
mod patch;
mod row;
mod store;
mod token;

pub use codec::{CodecError, decode, encode};
pub use frame::{
    CloseReason, Code, Downstream, FailedCode, FaultCode, Outcome, ResetReason, Upstream,
    WireAnchor, WireWindow,
};
pub use patch::{ApplyError, PatchOp, apply};
pub use row::WireRow;
pub use sse::SseEvent;
pub use store::{StoreError, WireStore};
pub use token::{ConnectionToken, Ft, Occ, OperationId, Sub};

// The engine renders payloads to `serde_json::Value`; re-export it so consumers
// construct and read wire values without pinning their own serde_json build.
pub use serde_json::{self, Value};
