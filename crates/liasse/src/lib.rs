//! Liasse is a Rust-first application state engine. A package declares typed
//! logical application state together with the operations and external
//! interfaces that observe or change it; a runtime loads that package, admits
//! mutations as atomic commits at one serial position each, evaluates read-only
//! views, and drives live clients with coherent patches — while remaining free
//! to choose its own storage, caching, and process placement (SPEC.md, Liasse
//! v0.5).
//!
//! This crate is the **facade**: it adds no logic of its own, only a curated,
//! documented re-export of the workspace's layers as a single dependency. A
//! Rust host author depends on `liasse` and reaches every embedding type through
//! one of the modules below.
//!
//! # The embedding model
//!
//! An embedding is a small stack of owned values — no globals, no interior
//! mutability, no reference counting:
//!
//! 1. **A store** ([`store`]) holds committed state and a replayable commit log.
//!    [`store::MemoryStore`] is the in-process reference implementation;
//!    [`store::PgStore`] is the PostgreSQL one. Both satisfy the same
//!    [`store::InstanceStore`] contract, so the layers above are storage-blind.
//! 2. **An engine** ([`engine`]) owns the store and a validated
//!    [`model::Model`]. [`engine::Engine::load`] parses a definition, seeds
//!    genesis, and activates the package; from there it admits
//!    [`engine::CallRequest`]s and evaluates views. Admission draws `now()` and
//!    the identifier seed once, from a [`engine::Generators`] — supply
//!    [`engine::FixedGenerators`] (or a [`surface::VirtualClock`]) to make a run
//!    deterministic and replayable.
//! 3. **A surface host** ([`surface`]) is the external interface over the
//!    engine. [`surface::SurfaceHost`] routes a client's dotted call/watch
//!    through the model's *exposed* surfaces only, authenticates and gates by
//!    role, and drives each subscription through the commit its call produced,
//!    so a `watch` stays coherent with the `call` that changed it.
//!
//! Every user-facing error is a [`diagnostics::Diagnostic`]. Portable
//! `.liasse` archives are built and opened through [`artifact`], and Rust host
//! components (namespaces, key providers, blob connectors) register through
//! [`host`].
//!
//! # A complete application, end to end
//!
//! The following is the SPEC.md §3.2 tasks package, driven through the surface
//! path exactly as §3.3 describes: load the definition, `watch` the public
//! view, `call` `add`, and observe the committed row on the same connection —
//! against [`store::MemoryStore`] under a fixed clock, so the run is
//! deterministic. `add_task` is written as the CORE bare-insert root mutation
//! (the local-binding `task = …; return task {…}` form the §3.2 listing shows is
//! an unimplemented runtime seam today); the §3.3 result shape is therefore read
//! back through the `open_tasks` view the same call swept the subscription
//! through.
//!
//! ```
//! use std::collections::BTreeMap;
//!
//! use liasse::ident::InstanceId;
//! use liasse::store::MemoryStore;
//! use liasse::surface::{
//!     CallBinding, Engine, Subscription, SurfaceAddress, SurfaceBinding, SurfaceCall,
//!     SurfaceHost, SurfaceOutcome, SurfaceRouterBuilder, SurfaceWatch, VirtualClock, ViewBinding,
//! };
//! use liasse::value::{Precision, Text, Value};
//!
//! // The §3.2 package: a keyed `tasks` collection with a generated id and a
//! // defaulted timestamp, one normalized+checked field, mutation programs, a
//! // filtered+sorted view, and a public surface exposing them.
//! const TASKS: &str = r#"{
//!   "$liasse": 1
//!   "$app": "example.tasks@1.0.0"
//!   "$model": {
//!     "tasks": {
//!       "$key": "id"
//!       "id": "uuid = uuid()"
//!       "title": {
//!         "$type": "text"
//!         "$normalize": "string.trim(.)"
//!         "$check": ["size(.) > 0", "A title is required"]
//!       }
//!       "done": "bool = false"
//!       "created_at": "timestamp = now()"
//!       "$mut": {
//!         "complete": [
//!           ".done = true"
//!           "return . { id, title, done, created_at }"
//!         ]
//!       }
//!     }
//!     "$mut": {
//!       "add_task": ".tasks + { title: @title }"
//!     }
//!     "open_tasks": {
//!       "$view": ".tasks[:task | !task.done] { id, title, created_at, $sort: [-created_at] }"
//!     }
//!     "$public": {
//!       "tasks": {
//!         "$view": ".open_tasks"
//!         "$mut": {
//!           "add": ".add_task"
//!           "complete": ".tasks[@id].complete()"
//!         }
//!       }
//!     }
//!   }
//! }"#;
//!
//! // A deterministic clock is both the engine's `Generators` (it fixes `now()`
//! // and the `uuid()` seed) and the surface layer's expiry clock.
//! let mut clock = VirtualClock::new(1_700_000_000_000_000, Precision::Micros);
//! let engine: Engine<MemoryStore> =
//!     Engine::load(MemoryStore::new(InstanceId::new("tasks")), TASKS, &mut clock)
//!         .expect("the §3.2 package loads and activates");
//!
//! // Wire the public surface to its runtime mutation and view. The builder
//! // re-validates every binding against the model's exposed surfaces.
//! let public_tasks = SurfaceBinding::new()
//!     .with_view(ViewBinding::new("open_tasks"))
//!     .with_call("add", CallBinding::root("add_task", ["title".to_owned()]));
//! let router = SurfaceRouterBuilder::new()
//!     .public_surface("tasks", public_tasks)
//!     .build(engine.model())
//!     .expect("the bindings validate against the model");
//!
//! let mut host = SurfaceHost::new(engine, router, clock);
//! host.connect("client");
//!
//! // `watch public.tasks` — the open-tasks view is empty at genesis.
//! let address = |dotted: &str| SurfaceAddress::parse(dotted).expect("a well-formed address");
//! let watch = SurfaceWatch::new(address("public.tasks"), "open");
//! let init = host.watch("client", &watch).expect("watch is granted");
//! assert!(matches!(init, Subscription::Init(ref view) if view.is_empty()));
//!
//! // `call public.tasks.add { title: "  Read the specification  " }`.
//! let mut args = BTreeMap::new();
//! args.insert("title".to_owned(), Value::Text(Text::new("  Read the specification  ")));
//! let call = SurfaceCall::new(address("public.tasks.add"), args);
//! let outcome = host.call("client", &call).expect("the call reaches admission");
//! assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "add commits: {outcome:?}");
//!
//! // §3.3: every live view on the same connection has advanced through that
//! // commit, so the row is already visible on the watch, evaluated from
//! // committed state — the §3.3 result shape. `$normalize` trimmed the title,
//! // and `id`/`created_at` were generated once for the admitted request.
//! let view = host.read_view("client", "open").expect("the subscription refreshed");
//! assert_eq!(view.len(), 1);
//! let row = &view.rows()[0];
//! assert_eq!(row.field("title"), Some(&Value::Text(Text::new("Read the specification"))));
//! assert!(matches!(row.field("id"), Some(Value::Uuid(_))), "a uuid was generated for the key");
//! assert!(matches!(row.field("created_at"), Some(Value::Timestamp(_))), "created_at was defaulted from now()");
//! // The row is in `open_tasks` precisely because its `done` field defaulted to
//! // false — the view's `!task.done` filter is that assertion (§3.3: done: false).
//! ```

#![forbid(unsafe_code)]

/// Source spans, diagnostics, and rustc-style rendering — every user-facing
/// error in the workspace is a [`Diagnostic`](diagnostics::Diagnostic) from
/// this layer (SPEC.md §2.6).
pub mod diagnostics {
    pub use liasse_diag::*;
}

/// Canonical typed values and their types — the Annex A/B value layer that every
/// other layer speaks at its boundaries, never bare scalars.
pub mod value {
    pub use liasse_value::*;
}

/// Opaque, canonical identities — instance, lineage, history point, row
/// incarnation, definition digest (SPEC.md §4, Annex D).
pub mod ident {
    pub use liasse_ident::*;
}

/// The validated semantic package [`Model`](model::Model): a constructed model
/// is proof the package is statically valid (SPEC.md Part I–II static rules).
pub mod model {
    pub use liasse_model::*;
}

/// The runtime [`Engine`](engine::Engine): loads a package, seeds genesis,
/// admits mutations as atomic commits, evaluates views, and replays
/// deterministically (SPEC.md §5, §8, §9, §22).
pub mod engine {
    pub use liasse_runtime::*;
}

/// The typed storage contract and its implementations: the reference
/// [`MemoryStore`](store::MemoryStore) and the PostgreSQL
/// [`PgStore`](store::PgStore), both satisfying one
/// [`InstanceStore`](store::InstanceStore) contract (SPEC.md §22, §23).
pub mod store {
    pub use liasse_pg::{PgStore, PgStoreFactory, PgTransition};
    pub use liasse_store::*;
}

/// External surfaces over the engine: the [`SurfaceHost`](surface::SurfaceHost),
/// routing, authentication, sessions, and live-view coherence (SPEC.md §10–12).
pub mod surface {
    pub use liasse_surface::*;
}

/// The portable `.liasse` archive: container, manifest, checksums, packing, and
/// package compatibility (SPEC.md §4.1, §19.5, Annex D/E).
pub mod artifact {
    pub use liasse_artifact::*;
}

/// The typed contract for registered Rust host components — namespaces, key
/// providers, blob connectors — and the [`Registry`](host::Registry) that
/// resolves a package's requirements (SPEC.md §16–18, §23).
pub mod host {
    pub use liasse_host::*;
}
