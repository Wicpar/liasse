//! Transport bindings over the sync [`ConnectCore`](crate::ConnectCore).
//!
//! The core is transport-agnostic; a binding only marshals bytes to and from it. The
//! reference binding ([`std_http`]) is blocking `std::net` HTTP/1.1 + SSE, behind the
//! default `std-http` feature and depending on nothing outside `std`. A `tokio`/axum
//! adapter is a later stage.

mod http;
pub mod std_http;

pub use std_http::{Server, serve};
