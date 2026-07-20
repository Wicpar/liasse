//! Compile-time pin for the store-backed engine's thread-safety, completing the
//! `Send + Sync` policy across the two `InstanceStore` implementations.
//!
//! The reference [`Engine<MemoryStore>`](liasse_runtime::Engine) is pinned
//! `Send + Sync` in `liasse-runtime`'s `send_sync` module. This crate is the only
//! one that sees both the runtime and the PostgreSQL store, so it pins the pg
//! side here.
//!
//! [`PgStore`](liasse_pg::PgStore) is `Send` but **not** `Sync`: its single
//! writer is a `postgres::Client` whose inner message stream
//! (`Pin<Box<dyn Stream + Send>>`) carries no `Sync` bound (one writer per
//! instance, §5.2). No `dyn` trait object is responsible for this — every boxed
//! contract the engine reaches is `Send + Sync`; the store's own writer handle is
//! what caps `Engine<PgStore>` at `Send`. That is the intended shape: a durable
//! instance is moved between threads, never shared by reference across them.

use liasse_pg::PgStore;
use liasse_runtime::Engine;

const _: fn() = || {
    fn assert_send<T: Send>() {}

    // A pg-backed engine is `Send` (movable between threads). It is deliberately
    // not asserted `Sync`: the single-writer `postgres::Client` is `!Sync`.
    assert_send::<Engine<PgStore>>();
    assert_send::<PgStore>();
};
