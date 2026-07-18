//! The Liasse client-sync connector (SPEC.md §12): a transport-agnostic sync core
//! over a [`SurfaceHost`](liasse_surface::SurfaceHost), plus a reference SSE + HTTP
//! transport binding.
//!
//! # What this crate adds
//! The surface layer already does the hard §12 work — live watches, the §12.3
//! completion barrier, windowed diffs, per-frontier re-authorization. The connector
//! adds only three things on top, and nothing that holds authority:
//!
//! - **Framing.** Inbound wire frames ([`liasse_wire::Upstream`]) are decoded — as
//!   hostile input, strictly against the model's declared types — into typed surface
//!   requests; outbound results are projected to [`liasse_wire::Downstream`] frames
//!   and the request [`liasse_wire::Outcome`].
//! - **Opaque identity.** A row's internal `RowId`, a `CommitSeq`, and a session are
//!   never on the wire; each is a per-connection capability token minted here (an
//!   [`Occ`](liasse_wire::Occ), an [`Ft`](liasse_wire::Ft), a connection token), and a
//!   forged one is a fault, never a panic or a leak.
//! - **The §12.2 stream.** One logical connection is the §12.3 coherence unit: a
//!   bounded SSE stream of `init`/`patch`/`close`/`frontier` frames (the SSE `id:` is
//!   the frontier token, giving `Last-Event-ID` resume) plus HTTP requests.
//!
//! # Trust boundary
//! The core is [`ConnectCore`]: a single-owner, `&mut self`, no-interior-mutability
//! object driven one request at a time. All authorization, projection, and admission
//! stay in the host; the client receives only the already-authorized projection
//! (AGENTS.md). The default [`bind::std_http`] binding wraps the core in an actor
//! thread; the conformance suite drives it in process.
//!
//! # D6 delta reconstruction
//! Coherent §12.2 patches are rebuilt without any runtime `ViewDelta`: the layer
//! retains the exact wire rows each client holds and, after a commit, diffs that
//! snapshot against the freshly projected authorized view. Because the occurrence
//! token is a stable relabeling of the internal `RowId`, this wire-level diff is the
//! runtime diff modulo that relabeling — the surface API stays untouched.

pub mod core;
pub mod decode;
pub mod encode;
pub mod error;
pub mod mount;
pub mod token;

#[cfg(feature = "std-http")]
pub mod bind;

pub use crate::core::{ConnectCore, Reply};
pub use decode::DecodeError;
pub use error::ConnectError;
pub use mount::{Schema, SchemaBuilder};
pub use token::{ConnKeys, TokenMinter, UnsignedMinter};
