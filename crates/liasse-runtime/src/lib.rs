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
//! clear, `assert`, `return`), seed admission, root views, the virtual clock, and
//! lifecycle buckets. Nested collections, view-sourced insert/replace, local
//! bindings, internal calls, host-requirement resolution against a registry, and
//! full dependency-ordered default evaluation remain documented seams. So do the
//! remaining feature families, each blocked on machinery outside this crate:
//! source-backed/recurring buckets and the `.$at`/`.$between`/`.$all` selectors
//! (§14.4–§14.6, expression-layer selectors and source derivation); meters
//! (§15, the `$quantity` pool projection and `spend.funding` accessor the
//! expression layer does not expose); keyrings and blobs (§17/§18, `liasse-host`
//! providers/connectors); history export/import/reconcile (§19,
//! `liasse-artifact` builders); migrations (§20); erasure (§21); and module
//! composition (§13, multi-instance store composition).

mod bucket;
mod compiled;
mod doc;
mod engine;
mod env;
mod error;
mod eval;
mod generator;
mod interp;
mod materialize;
mod outcome;
mod request;
mod response;
mod rules;
mod schema;
mod scope;
mod seed;
mod state;
mod view;

pub use engine::Engine;
pub use error::{EngineError, Rejection, RejectionReason};
pub use generator::{derive_uuid, FixedGenerators, Generators};
pub use outcome::CallOutcome;
pub use request::CallRequest;
pub use response::ResponseValue;
pub use view::{ViewDelta, ViewResult, ViewRow};

/// Re-exported so callers build typed requests and read outcomes without a
/// direct dependency edge on the value and store crates.
pub use liasse_store::CommitSeq;
pub use liasse_value::{Precision, Timestamp, Value};
