//! Runtime: the engine that loads a validated package into a store, seeds it,
//! admits mutations as atomic commits, and replays deterministically (SPEC.md
//! §5, §8, §9, §22).
//!
//! # What this crate owns
//!
//! - [`Engine`] — owns the store, the validated [`Model`](liasse_model::Model),
//!   and the compiled defaults/mutations/views. It loads a definition, admits a
//!   genesis commit with `$data` seeds through the full rule pipeline (§9.1),
//!   admits mutation calls at one serial position each (§22.2), evaluates views
//!   at a frontier, and is rebuilt deterministically from the store's log.
//! - The admission pipeline (§8/§5): a mutation program executes statement by
//!   statement against a prospective state; then defaults, normalization,
//!   checks, and key/ref/uniqueness enforcement run over the final prospective
//!   state; any failure aborts with a typed [`Rejection`] and the store is
//!   untouched; success commits and the `return` is evaluated from committed
//!   state (§8.6).
//! - [`CallOutcome`] — the typed request outcome vocabulary (`committed`,
//!   `unchanged`, `rejected`) mirroring the corpus (§9.4, §22.7).
//! - [`ViewResult`] / [`ViewDelta`] — view evaluation at a [`CommitSeq`] and the
//!   minimal init/patch delta between frontiers (§12.4).
//! - [`Generators`] — the per-request `now()`/`uuid()` source the engine samples
//!   once and records, the seam that makes admission deterministic (§8.12, A.5).
//!
//! # Determinism and replay
//!
//! The store guarantees a gapless, replayable commit log; the engine guarantees
//! that admission writes every generated and sampled value into the committed
//! ops. Rebuilding an engine over the same store therefore reproduces state
//! exactly, and re-running the same request sequence under the same
//! [`Generators`] yields byte-identical committed state.
//!
//! # Virtual clock and buckets (§14)
//!
//! The engine owns a virtual clock ([`Engine::now`], [`Engine::set_time`],
//! [`Engine::advance`]): `now()` samples it and bucket activity is evaluated
//! against it, so temporal reads are deterministic and advance only explicitly.
//! Lifecycle buckets (§14.1–§14.2) are enforced through the same atomic pipeline:
//! a `$bucket` interval `[from, until)` filters a collection's ordinary reads and
//! views to the rows active at the clock instant (half-open, so a row leaves
//! every active view at the exact `until` instant while remaining extant), and a
//! transition producing an invalid finite interval is rejected at admission.
//!
//! # CORE scope and documented seams
//!
//! CORE covers top-level keyed collections with scalar/ref/set fields, row and
//! root mutations (assign, keyed insert, keyed delete, keyed single-row patch,
//! clear, `assert`, `return`), seed admission, root views, the virtual clock,
//! lifecycle buckets, and the `.$at`/`.$between`/`.$all` temporal selectors over
//! bucketed collections (§14.1–§14.2). Nested collections, view-sourced
//! insert/replace, local bindings, internal calls, host-requirement resolution
//! against a registry, and full dependency-ordered default evaluation remain
//! documented seams. So do the remaining feature families, each blocked on
//! machinery outside this crate's current reach:
//!
//! - **Source-backed and recurring buckets** (§14.4–§14.6): deriving interval
//!   rows from a `$source` view needs a source-materialization pass, and
//!   recurring calendar periods need period-to-timestamp arithmetic (zone/DST/
//!   overflow) that `liasse-value` does not yet expose. Fixed-period recurrence
//!   is unblocked but not yet built.
//! - **Meters** (§15): `$limits`/`$consumes`/`$sources` allocation, `$eligible`,
//!   `$order` draining, `spend.funding`, and the `.credits.balance`/`.pools`
//!   accessors. The `$quantity` pool projection and the temporal selectors this
//!   stage landed are the expression-layer prerequisites; the remaining blocker
//!   is *nested* collections — every §15 spend/pool arrangement places the spend
//!   and pool collections under an ancestor row (`spends`/`topups` inside
//!   `users`/`accounts`), which the top-level-only materializer, interpreter, and
//!   `Prospective` do not yet handle. Allocation must run inside atomic admission
//!   so an exhausted meter rejects the whole transition and a failed program
//!   releases every provisional allocation; that lands with nested collections.
//! - history export/import/reconcile (§19, `liasse-artifact` builders);
//!   migrations (§20); and module composition (§13, multi-instance store).
//!
//! # Keyrings, blobs, deletion, and erasure (§17/§18/§21)
//!
//! These three feature families land as self-contained dynamic-semantics
//! modules over the `liasse-host` provider/connector contracts and the engine's
//! virtual clock, each exercised against the host doubles:
//!
//! - [`Keyring`] (§17): the version lifecycle, rotation scheduling on the
//!   virtual clock, sealed public-only metadata, and §17.9 failure keep-current
//!   over a [`KeyProvider`](liasse_host::KeyProvider).
//! - [`BlobEngine`] (§18): descriptor acceptance, placement-policy planning,
//!   transactional upload, and integrity-verified fetch over a
//!   [`BlobConnector`](liasse_host::BlobConnector), so tampered bytes never
//!   surface.
//! - [`Graph`]/[`Erasure`] (§21): the cascade deletion plan and erasure that
//!   scrubs retained payloads to digest stubs while keeping history verifiable.
//!
//! Persisting keyring versions and blob-placement rows as store-backed
//! application state, and threading these through the mutation admission
//! pipeline and history log, remains a seam blocked on store-contract
//! extensions (a durable version/placement schema and a history-scrub hook);
//! the modules pin the observable semantics the corpus re-derives.

mod blobs;
mod bucket;
mod cascade;
mod compiled;
mod deletion;
mod doc;
mod engine;
mod env;
mod error;
mod eval;
mod generator;
mod history;
mod interp;
mod keyring;
mod materialize;
mod migrate;
mod modules;
mod outcome;
mod portable;
mod request;
mod response;
mod rules;
mod schema;
mod scope;
mod seed;
mod singleton;
mod state;
mod view;

pub use blobs::{
    AcceptedType, Blob, BlobEngine, CopyState, DeclaredDescriptor, FetchError, Placement, Store,
    StoreId, UploadError,
};
pub use deletion::{
    DeleteError, DeletePolicy, DeletionPlan, Erasure, Extract, Graph, Occurrence, RefEdge, RowRef,
};
pub use engine::Engine;
pub use error::{EngineError, Rejection, RejectionReason};
pub use generator::{derive_uuid, FixedGenerators, Generators};
pub use history::{
    ConflictKind, ImportError, ImportRelation, ImportReport, MergeConflict, MergeOutcome,
};
pub use migrate::{UpdateError, UpdateReport};
pub use modules::{ModuleError, ModuleHost, SeedMerge};

/// The Annex E version relationship an [`UpdateReport`] carries (§20.3).
pub use liasse_artifact::UpdateRelation;
pub use keyring::{
    KeyState, KeyVersion, Keyring, KeyringError, KeyringPolicy, RotationMode, RotationOutcome,
    RotationSchedule, SessionToken, VerifyError, VersionId,
};
pub use outcome::CallOutcome;
pub use request::CallRequest;
pub use response::ResponseValue;
pub use view::{ViewDelta, ViewResult, ViewRow};

/// Re-exported so callers build typed requests and read outcomes without a
/// direct dependency edge on the value and store crates.
pub use liasse_store::CommitSeq;
pub use liasse_value::{Precision, Timestamp, Value};
