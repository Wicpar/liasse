//! Compile-time pins for the workspace `Send + Sync` policy.
//!
//! Maintainer directive: *"when using `dyn` ensure they are always `Send` and
//! `Sync` so the engine can be `Send` and `Sync`."* Every `dyn` trait object the
//! engine's field chain reaches carries a `Send + Sync` supertrait bound, so
//! [`Engine<S>`](crate::Engine) is `Send + Sync` whenever the store `S` is. The
//! three `dyn` objects the engine's [`Registry`](liasse_host::Registry) holds
//! (`HostNamespace`, `KeyProvider`, `BlobConnector`, §16.2) were the original
//! `!Send`/`!Sync` blockers; the store's `ViewProgram` (pushdown, §ViewProgram)
//! is the fourth boxed contract.
//!
//! These `const _` items are pure type assertions: they evaluate no code and
//! fail to *compile* if any of those supertrait bounds regress (e.g. a new field
//! that is `!Send`, or a dropped `Send + Sync` bound on a boxed trait). Their home
//! is recorded in `AGENTS.md`.

use crate::Engine;
use liasse_host::{BlobConnector, HostNamespace, KeyProvider};
use liasse_store::{MemoryStore, ViewProgram};

const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}

    // The whole point of the directive: an engine over a `Send + Sync` store is
    // itself `Send + Sync`. `MemoryStore` is the always-available witness.
    assert_send_sync::<Engine<MemoryStore>>();

    // The boxed `dyn` contracts the engine and store reach: dropping the
    // `Send + Sync` supertrait bound on any of these traits breaks this line.
    assert_send_sync::<Box<dyn HostNamespace>>();
    assert_send_sync::<Box<dyn KeyProvider>>();
    assert_send_sync::<Box<dyn BlobConnector>>();
    assert_send_sync::<Box<dyn ViewProgram>>();
};
