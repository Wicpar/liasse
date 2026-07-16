//! Surfaces: named external API entries, roles, authentication, sessions,
//! clients, and live-view coherence (SPEC.md Part II, §10–12).
//!
//! This crate is the external interface over the runtime [`Engine`]. It routes a
//! client's dotted call/watch through the model's *exposed* surfaces only,
//! authenticates and gates by role, drives live subscriptions with coherent
//! patches, and deduplicates operations by identifier — all as plain owned state
//! over the engine's public admission and view API.
//!
//! # Layering
//!
//! - **Resolution** ([`SurfaceRouter`], [`SurfaceAddress`]) — a dotted address
//!   resolves through the exposed public/role surfaces only; an internal or
//!   unexposed name is [`SurfaceOutcome::Denied`] `Unresolved` (§10.1, §12.1).
//!   A [`SurfaceRouterBuilder`] re-validates every binding against
//!   [`liasse_model::Model::surfaces`], so only exposed, declared members route.
//! - **Authentication** ([`Authenticator`], [`SessionAuthenticator`], [`Role`])
//!   — an authenticator verifies a credential and resolves an actor/session
//!   against committed state; a role gates by accepted authenticator (§11.4) and
//!   membership re-evaluated at admission (§10.3). Session expiry is judged
//!   against the engine's virtual clock (§11.7).
//! - **Clients** ([`SurfaceHost`], [`Connection`], [`Watch`]) — a connection
//!   owns its subscriptions and a frontier; a successful call advances that
//!   frontier through at least the returned commit and drags every still-
//!   authorized subscription through it, delivering coherent patches over the
//!   engine's [`liasse_runtime::ViewDelta`] primitive (§12.2–§12.6).
//! - **Operations** ([`OperationLog`]) — a retained record per scoped identifier
//!   provides at-most-once execution: an equivalent retry re-observes the stored
//!   outcome, a divergent reuse is rejected (§12.3).
//!
//! # Documented seams
//!
//! The model validates surface/authenticator declarations but retains neither
//! the `$mut`→mutation wiring nor the executable `$verify`/`$members`/`$actor`
//! expressions (`crates/liasse-model/src/{surface,auth}.rs` leave those as later
//! passes). This layer supplies those seams as explicit host wiring — a
//! [`Verifier`] for `$verify`, a [`RowSource`] for `$session`/`$actor`/
//! `$members` — validated against the model's exposure boundary. Bounded windows
//! (§12.2), recursive surface coverage (§10.5), and bucket-driven temporal
//! frontier observation (§12.2) are left out of this core.
//!
//! [`Engine`]: liasse_runtime::Engine

mod address;
mod authn;
mod binding;
mod clock;
mod connection;
mod host;
mod operation;
mod outcome;
mod reader;
mod request;
mod role;
mod router;
mod watch;

pub use address::{AddressError, Authority, SurfaceAddress};
pub use authn::{
    Actor, AuthContext, Authenticator, Claims, Credential, RowLookup, RowSource, Session,
    SessionAuthenticator, SessionSource, Verifier, VerifyFailure,
};
pub use binding::{CallBinding, SurfaceBinding, ViewBinding};
pub use clock::VirtualClock;
pub use connection::{Connection, DEFAULT_CONTEXT};
pub use host::{AuthResult, Subscription, SurfaceError, SurfaceHost};
pub use operation::{OperationKey, OperationLog, OperationStatus, RequestModel};
pub use outcome::{Completion, Denial, DenialReason, SurfaceOutcome};
pub use reader::{EngineReader, StateReader};
pub use request::{Authenticate, AuthSelection, SurfaceCall, SurfaceWatch};
pub use role::Role;
pub use router::{Resolved, RouterError, SurfaceRouter, SurfaceRouterBuilder};
pub use watch::{Watch, WatchAuthz};

/// Re-exported so callers build requests, read outcomes, and inspect views
/// without a direct dependency edge on the runtime and value crates.
pub use liasse_runtime::{
    CommitSeq, Engine, Precision, Rejection, ResponseValue, Timestamp, Value, ViewDelta, ViewResult,
    ViewRow,
};
