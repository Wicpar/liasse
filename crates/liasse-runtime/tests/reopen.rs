#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Durable REOPEN over the in-memory reference store: a durable engine reopens
//! its own persisted store at the CURRENT head WITHOUT re-running genesis.
//!
//! Booting an instance a second time over an already-populated store is what a
//! process restart does. The genesis-load paths (`load*`) always seed
//! `$data`/`$bundle`, so a second genesis over a populated store collides on the
//! already-present seed rows (`row /accounts/001 already occupied`). A reopen
//! adopts the store's head cursor and applies no seed, so it reads the committed
//! state and continues from `head + 1`. A reopen whose definition is not the
//! installed package (a different identity or version) fails loudly rather than
//! re-seeding or attaching a foreign shape.

mod support;

use liasse_ident::InstanceId;
use liasse_runtime::{CallRequest, Engine, EngineError, Registry, Value};
use liasse_store::{CommitSeq, MemoryStore};
use liasse_value::Text;
use support::{generator, NOW_MICROS};

/// A ledger whose genesis `$data` seeds account `001`; `open` adds an account, so
/// a commit advances the head past the genesis point.
const LEDGER_V1: &str = r#"{
  "$liasse": 1
  "$app": "example.ledger@1.0.0"
  "$model": {
    "accounts": { "$key": "id", "id": "text", "balance": "int = 0" }
    "all_accounts": { "$view": ".accounts { id, balance }" }
    "$mut": { "open": ".accounts + { id: @id }" }
  }
  "$data": { "accounts": { "001": { "balance": 100 } } }
}"#;

/// The SAME package at a NEW version — a §20 migration, never a reopen.
const LEDGER_V2: &str = r#"{
  "$liasse": 1
  "$app": "example.ledger@2.0.0"
  "$model": {
    "accounts": { "$key": "id", "id": "text", "balance": "int = 0" }
    "all_accounts": { "$view": ".accounts { id, balance }" }
    "$mut": { "open": ".accounts + { id: @id }" }
  }
  "$data": { "accounts": { "001": { "balance": 100 } } }
}"#;

/// A DIFFERENT package entirely.
const OTHER_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.other@1.0.0"
  "$model": {
    "accounts": { "$key": "id", "id": "text", "balance": "int = 0" }
    "all_accounts": { "$view": ".accounts { id, balance }" }
    "$mut": { "open": ".accounts + { id: @id }" }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// The `(id, balance)` pairs the `all_accounts` view reports at the head, balance
/// rendered as its canonical decimal text.
fn accounts(engine: &Engine<MemoryStore>) -> Vec<(String, String)> {
    engine
        .view_at_head("all_accounts")
        .expect("view read")
        .expect("view declared")
        .rows()
        .iter()
        .map(|row| {
            let id = match row.field("id").expect("id") {
                Value::Text(t) => t.as_str().to_owned(),
                other => panic!("id is not text: {other:?}"),
            };
            let balance = match row.field("balance").expect("balance") {
                Value::Int(n) => n.to_canonical_text(),
                other => panic!("balance is not int: {other:?}"),
            };
            (id, balance)
        })
        .collect()
}

fn open_account(engine: &mut Engine<MemoryStore>, id: &str) {
    let mut generator = generator();
    let request = CallRequest::new("open").arg("id", text(id));
    engine.call(&request, &mut generator).expect("open commits");
}

/// Genesis-load engine A, commit a mutation to advance the head past genesis,
/// then REOPEN a fresh engine B over the SAME store: B reads the committed rows,
/// the head is preserved (not reset to genesis), no seed collision fires, and a
/// further mutation on B commits at `head + 1`.
#[test]
fn reopen_reads_head_and_continues_without_reseeding() {
    let instance = "ledger-reopen";
    let mut generator = generator();

    // Engine A: genesis seeds account 001, then `open` adds 002 (head advances).
    let store = MemoryStore::new(InstanceId::new(instance));
    let mut a = Engine::load(store, LEDGER_V1, &mut generator).expect("genesis load");
    open_account(&mut a, "002");
    let head_before = a.head().expect("head A");
    assert!(head_before > CommitSeq::GENESIS, "the commits advanced the head past genesis");
    assert_eq!(
        accounts(&a),
        vec![("001".to_owned(), "100".to_owned()), ("002".to_owned(), "0".to_owned())],
        "engine A holds the seeded and the opened account",
    );

    // Hand the SAME populated store to a fresh engine B via reopen: no genesis, no
    // re-seed, the head is adopted verbatim.
    let store = a.into_store();
    let mut b = Engine::reopen_with_hosts(store, LEDGER_V1, &mut generator, Registry::new())
        .expect("reopen over the populated store");

    assert_eq!(b.head().expect("head B"), head_before, "the reopened head is the persisted head, not genesis");
    assert_eq!(
        accounts(&b),
        vec![("001".to_owned(), "100".to_owned()), ("002".to_owned(), "0".to_owned())],
        "the reopened engine reads the committed rows",
    );

    // A further mutation on B commits at head + 1 — the store continues from the
    // persisted head, it did not restart.
    open_account(&mut b, "003");
    assert_eq!(b.head().expect("head B after"), head_before.next(), "the mutation commits at head + 1");
    assert_eq!(
        accounts(&b),
        vec![("001".to_owned(), "100".to_owned()), ("002".to_owned(), "0".to_owned()), ("003".to_owned(), "0".to_owned())],
        "the new account joins the reopened state",
    );
}

/// The unified boot path `open_with_hosts` genesis-loads a FRESH store and
/// REOPENs an EXISTING one, so first boot and every restart go through one call.
#[test]
fn open_dispatches_fresh_to_genesis_and_existing_to_reopen() {
    let instance = "ledger-open";
    let mut generator = generator();

    // First boot: a fresh store is genesis-loaded and seeded.
    let store = MemoryStore::new(InstanceId::new(instance));
    let mut first = Engine::open_with_hosts(store, LEDGER_V1, &mut generator, Registry::new())
        .expect("first boot genesis-loads a fresh store");
    open_account(&mut first, "002");
    let head_before = first.head().expect("head");
    let store = first.into_store();

    // Restart: the SAME store is now populated, so the identical call reopens it
    // instead of re-seeding — no collision.
    let restarted = Engine::open_with_hosts(store, LEDGER_V1, &mut generator, Registry::new())
        .expect("restart reopens the existing store");
    assert_eq!(restarted.head().expect("head"), head_before, "restart preserved the head");
    assert_eq!(
        accounts(&restarted),
        vec![("001".to_owned(), "100".to_owned()), ("002".to_owned(), "0".to_owned())],
        "restart reads the committed state, unre-seeded",
    );
}

/// Reopening with a definition whose package version differs from the installed
/// one is a §20 migration, not a reopen: it fails loudly.
#[test]
fn reopen_with_mismatched_version_is_a_loud_error() {
    let instance = "ledger-mismatch-version";
    let mut generator = generator();
    let store = MemoryStore::new(InstanceId::new(instance));
    let a = Engine::load(store, LEDGER_V1, &mut generator).expect("genesis load");
    let store = a.into_store();

    let Err(error) = Engine::reopen_with_hosts(store, LEDGER_V2, &mut generator, Registry::new())
    else {
        panic!("a version change must not reopen");
    };
    match error {
        EngineError::Mismatch(message) => {
            assert!(message.contains("example.ledger@1.0.0"), "names the installed version: {message}");
            assert!(message.contains("example.ledger@2.0.0"), "names the supplied version: {message}");
        }
        other => panic!("expected EngineError::Mismatch, got {other:?}"),
    }
}

/// Reopening with an entirely different package fails loudly rather than
/// attaching the foreign shape.
#[test]
fn reopen_with_foreign_package_is_a_loud_error() {
    let instance = "ledger-mismatch-identity";
    let mut generator = generator();
    let store = MemoryStore::new(InstanceId::new(instance));
    let a = Engine::load(store, LEDGER_V1, &mut generator).expect("genesis load");
    let store = a.into_store();

    let Err(error) = Engine::reopen_with_hosts(store, OTHER_APP, &mut generator, Registry::new())
    else {
        panic!("a foreign package must not reopen");
    };
    assert!(matches!(error, EngineError::Mismatch(_)), "expected a loud mismatch, got {error:?}");
}

/// Reopening a fresh, never-installed store fails loudly: there is nothing to
/// continue, and a reopen must never silently fall back to a genesis seed.
#[test]
fn reopen_on_a_fresh_store_is_a_loud_error() {
    let instance = "ledger-fresh";
    let mut generator = generator();
    let store = MemoryStore::new(InstanceId::new(instance));

    let Err(error) = Engine::reopen_with_hosts(store, LEDGER_V1, &mut generator, Registry::new())
    else {
        panic!("a fresh store has nothing to reopen");
    };
    assert!(matches!(error, EngineError::Mismatch(_)), "expected a loud mismatch, got {error:?}");

    // Sanity: the constant is the micro-precision boot instant the fixtures use.
    assert_eq!(NOW_MICROS, 1_700_000_000_000_000);
}
