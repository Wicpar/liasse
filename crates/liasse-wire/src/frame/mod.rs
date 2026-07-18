//! The wire frame vocabulary, grouped by direction: [`Downstream`] (server to
//! client, over the SSE stream), [`Upstream`] (client to server, request bodies),
//! and [`Outcome`] (the spec result of a request). Every frame is a plain serde
//! type with stable field names, codec-agnostic — the JSON codec ([`crate::codec`])
//! is one rendering, and a CBOR seam can render the same types later.

mod downstream;
mod outcome;
mod upstream;

pub use downstream::{CloseReason, Downstream, FaultCode, ResetReason};
pub use outcome::{Code, FailedCode, Outcome};
pub use upstream::{Upstream, WireAnchor, WireWindow};
