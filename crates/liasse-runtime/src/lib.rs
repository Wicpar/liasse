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
//! insert/replace, local bindings, internal calls, and full dependency-ordered
//! default evaluation remain documented seams. So do the remaining feature
//! families, each blocked on machinery outside this crate's current reach:
//!
//! # Host namespaces and keyring calls (§16, §17.7/§17.8)
//!
//! A package's `$requires` host-namespace declarations are resolved against a
//! host [`Registry`] the engine holds ([`Engine::load_with_hosts`]):
//! a missing/incompatible/ambiguous requirement fails before activation (§16.2,
//! §9.2 step 4); the default [`Engine::load`] manages no components and defers an
//! unresolved requirement instead. A mutation program's host-namespace call —
//! `util.double(...)` through the [`ConformanceGuard`]
//! (a nonconforming return is a rejection), or `cose.sign(/ring, claims)` through
//! the internally-provisioned [`Keyring`] (§17.7/§17.8: signing exercises the
//! active version, so a §17.9 outage rejects and mints no token) — is dispatched
//! by the interpreter (`host` module). A host-namespace call in an *expression*
//! position is typed and evaluated too: the runtime threads the resolved
//! `$requires` signatures into its checking [`scope`] (§16.2) and its evaluation
//! [`env`] dispatches a resolved call through the same [`ConformanceGuard`], with
//! the position's effect policy (§16.3/§8.8) deciding where each effect class may
//! run — pure in a view/computed, generated in a default/mutation value, verifier
//! at admission. The model's Phase-2 checker now agrees: `compile_definition`
//! feeds the same resolved signatures into
//! [`Model::build_with_hosts`](liasse_model::Model::build_with_hosts), so its
//! `check_tree`/`ModelScope` types a `$view`/`$default`/computed/`$check`/
//! `$normalize` host call against the pinned contract and effect policy instead of
//! rejecting it as an unknown function before activation. A `$mut` operator-value
//! host call (an insert object member) is still accepted structurally by the model
//! and typed by the compiled layer, as before.
//!
//! - **Source-backed and recurring buckets** (§14.4–§14.6) are implemented
//!   ([`source_bucket`]): a compiled pass reads each `$source`/`$from`/`$until`/
//!   `$repeat`/output declaration from the document, and materialization evaluates
//!   the source view, generates the interval series with `liasse-value`'s period
//!   arithmetic (`Period::advance`, `recurring_intervals`), and exposes each
//!   derived row's `$source`/`$from`/`$until`/`$index` bindings. Admission rejects a
//!   non-advancing or ill-bounded series (§14.5). Remaining seams: a **named
//!   calendar time zone** needs a tzdb this offline build does not bundle (UTC and
//!   fixed periods are exact); and a **future window over an unbounded series**
//!   (`.$between(a, b)` with `b` beyond the read clock) is generated only to the
//!   read horizon, so far-future periods of an unbounded series are not yet
//!   enumerated.
//! - **Meters** (§15) are implemented ([`meter`]): a compiled-meter pass reads
//!   each `$limits`/`$consumes`/`$sources` declaration from the document (like
//!   [`compile_buckets`](crate::compiled)); admission funds every new or changed
//!   spend by resolving the reachable pools active at the spend `$time`,
//!   coalescing duplicate identities, gating by `$eligible`, draining in `$order`,
//!   and rejecting the whole transition on insufficient eligible capacity; the
//!   allocation is frozen onto the spend row as an admission fact, so deleting a
//!   spend releases it and updating it releases and reallocates. The §15.6
//!   accessors (`.<meter>.balance`/`.pools`, `spend.funding`) are folded onto the
//!   materialized row tree. Recurring source-backed pools (§14.5, W3's
//!   `credit_periods`) are funded through the [`source_bucket`] derivation, and
//!   §4.4's declared `timestamp_precision` is applied to stored `timestamp` field
//!   types at compile time so a bucketed pool bound and a spend `$time` compare at
//!   the intended scale. Remaining meter seams: the **parameterized §15.6
//!   accessor** (`.<meter>.balance({ $time })`) is context-free only; and a
//!   **nested-collection surface mutation** (`companies[c].accounts[a].consume`)
//!   needs the surface lift the runtime cannot reach. Nested-collection seeding
//!   (§5.5) and nested-collection keyed deletion, both prerequisites, are now
//!   handled.
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
//!   over a [`KeyProvider`].
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
mod contract;
mod deletion;
mod doc;
mod engine;
mod env;
mod error;
mod eval;
mod generator;
mod history;
mod host;
mod interp;
mod keyring;
mod keyring_view;
mod materialize;
mod meter;
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
mod source_bucket;
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
pub use host::CoseVerifyError;

/// Re-exported so a host or the testkit builds the component registry the engine
/// resolves `$requires` against ([`Engine::load_with_hosts`]) and drives the
/// keyring/provider fault-injection vocabulary (§16.2, §17).
pub use liasse_host::{
    ConformanceGuard, ContractName, ContractRef, CoseClaims, CoseToken, EffectClass, HostNamespace,
    InterfaceHash, KeyProvider, NamespaceDescriptor, OpSignature, Registry, Version,
};
pub use generator::{derive_uuid, FixedGenerators, Generators};
pub use history::{
    ConflictCoordinate, ConflictKind, ImportError, ImportRelation, ImportReport, MergeConflict,
    MergeOutcome,
};
pub use migrate::{UpdateError, UpdateReport};
pub use modules::{
    AdmittedBindings, DepSpec, InstallRequest, InterfaceRow, ModuleError, ModuleHost, ModuleSpace,
    SeedMerge, UseSpec,
};

/// The Annex E version relationship an [`UpdateReport`] carries (§20.3).
pub use liasse_artifact::UpdateRelation;
pub use keyring::{
    KeyState, KeyVersion, Keyring, KeyringError, KeyringPolicy, RotationMode, RotationOutcome,
    RotationSchedule, SessionToken, VerifyError, VersionId,
};
pub use keyring_view::MANUAL_EXTERNAL_KEY;
pub use outcome::CallOutcome;
pub use request::{CallRequest, ViewQuery};
pub use response::ResponseValue;
pub use view::{ViewDelta, ViewResult, ViewRow};

/// Re-exported so callers build typed requests and read outcomes without a
/// direct dependency edge on the value and store crates.
pub use liasse_store::CommitSeq;
pub use liasse_value::{Precision, RefKey, Timestamp, Value};
