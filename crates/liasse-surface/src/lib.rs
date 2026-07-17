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
//! `$members` — validated against the model's exposure boundary.
//!
//! Bounded windows ([`Window`], §12.2 `$size`/`$anchor`/`$slide`) and
//! resume-from-retained-frontier ([`SurfaceHost::resume`], §12.2) are supplied
//! here as client-side projections over the engine's per-frontier
//! [`ViewResult`]. Restart durability (§22) is [`SurfaceHost::into_parts`]: it
//! hands the sealed engine, router, and clock back so a driver can drop the host
//! — losing only its volatile connection/subscription/operation state — and
//! rebuild a fresh host over the same engine, whose committed store survives the
//! handoff untouched. The bucket clock (§14) advances through
//! [`SurfaceHost::advance_time`], which moves the engine's own virtual clock (not
//! only the surface expiry clock) so temporal reads reflect the new instant. Following one occurrence *across a rekey* (§12.2), recursive
//! surface coverage (§10.5), scoped-role views nested on rows (§10.3), and
//! bucket-driven temporal frontier observation (§12.2) each need identity or
//! scope-parameterized view evaluation the runtime's flat, key-derived
//! [`ViewResult`] does not yet expose, and are left to the runtime.
//!
//! [`Engine`]: liasse_runtime::Engine

mod address;
mod authn;
mod binding;
mod blobs;
mod clock;
mod connection;
mod cose;
mod host;
mod keyring;
mod modules;
mod operation;
mod outcome;
mod reader;
mod request;
mod role;
mod router;
mod watch;
mod window;

pub use address::{AddressError, Authority, SurfaceAddress};
pub use blobs::{BlobGetOutcome, BlobHost, BlobPutOutcome};
pub use cose::{CoseKeyring, CoseVerifyError};
pub use keyring::KeyringAdmin;
pub use modules::{ModuleDeployment, ModuleFault, ModuleObservation, ModuleUpdate};
pub use authn::{
    Actor, AuthContext, Authenticator, Claims, Credential, RowLookup, RowSource, Session,
    SessionAuthenticator, SessionSource, Verifier, VerifyFailure,
};
pub use binding::{CallBinding, SurfaceBinding, ViewBinding};
pub use clock::VirtualClock;
pub use connection::{Connection, DEFAULT_CONTEXT};
pub use host::{
    AuthResult, ChooseMap, ChooseSide, ConflictCoordinate, CorrectionError, CorrectionOutcome,
    EraseOutcome, HostComponentError, KeyringErrorOr, Subscription, SurfaceError, SurfaceHost,
    VerifyErrorOr,
};
pub use operation::{OperationKey, OperationLog, OperationStatus, RequestModel};
pub use outcome::{Completion, Denial, DenialReason, SurfaceOutcome};
pub use reader::{EngineReader, StateReader};
pub use request::{Authenticate, AuthSelection, SurfaceCall, SurfaceResume, SurfaceWatch};
pub use role::Role;
pub use router::{Resolved, RouterError, SurfaceRouter, SurfaceRouterBuilder};
pub use watch::{Watch, WatchAuthz};
pub use window::{Window, WindowError};

/// Re-exported so window anchors and view rows can be named without a direct
/// dependency edge on the expression crate.
pub use liasse_expr::RowId;

/// Re-exported so callers build requests, read outcomes, and inspect views
/// without a direct dependency edge on the runtime and value crates.
pub use liasse_runtime::{
    AcceptedType, AdmittedBindings, Blob, BlobEngine, CommitSeq, ConflictKind, CopyState,
    DeclaredDescriptor, DeleteError, DepSpec, Engine, Erasure, Extract, FetchError, ImportError,
    ImportRelation, ImportReport, InstallRequest, InterfaceRow, KeyState, KeyVersion, Keyring,
    KeyringError, KeyringPolicy, MergeConflict, MergeOutcome, ModuleError, ModuleHost, ModuleSpace,
    Occurrence, PatchOp, Placement, PlacementState, Precision, Rejection, ResponseValue, RotationMode,
    RotationOutcome, RotationSchedule, SeedMerge, SessionToken, Store, StoreId, Timestamp,
    UpdateReport, UploadError, UseSpec, Value, VerifyError, VersionId, ViewDelta, ViewResult,
    ViewRow,
};

/// Re-exported so a driver builds keyring providers, blob connectors, cose
/// claims/tokens, and their fault-injection scripts without a direct dependency
/// edge on the host crate.
pub use liasse_host::{
    cose_descriptor, BlobConnector, CoseClaims, CoseToken, ExternalKeyRef, KeyProvider,
};
