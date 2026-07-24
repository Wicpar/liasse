//! Durable REOPEN over the PostgreSQL store — the process-restart case.
//!
//! A durable engine boots once over a fresh schema (genesis seeds `$data`), then
//! a process restart opens a FRESH `PgStore` over the SAME instance/database and
//! must CONTINUE from the persisted head WITHOUT re-running genesis. Before the
//! engine gained a reopen path, the restart re-ran genesis over the populated
//! schema and collided on the seed (`row /accounts/001 already occupied`). The
//! store already reopens cleanly (`PgStoreFactory::reopen`); this proves the
//! engine reopen on top of it reads the persisted head + rows, applies no
//! re-seed, and continues — and that a version/identity mismatch fails loudly.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_ident::InstanceId;
use liasse_pg::PgStore;
use liasse_runtime::{CallRequest, Engine, EngineError, FixedGenerators, Precision, Registry, Value};
use liasse_store::{CommitSeq, StoreFactory};
use liasse_value::Text;
use support::SchemaGuard;

/// A fixed micro-precision instant used as the deterministic `now()` sample.
const NOW_MICROS: i128 = 1_700_000_000_000_000;

fn generator() -> FixedGenerators {
    FixedGenerators::new(NOW_MICROS, Precision::Micros)
}

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

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// The `(id, balance)` pairs the reopened engine's `all_accounts` view reports,
/// balance rendered as canonical decimal text — a durable SQL read.
fn accounts(engine: &Engine<PgStore>) -> Vec<(String, String)> {
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

fn open_account(engine: &mut Engine<PgStore>, id: &str) {
    let request = CallRequest::new("open").arg("id", text(id));
    engine.call(&request, &mut generator()).expect("open commits");
}

/// Genesis-load + commit over a PgStore, drop the engine (the process ends), then
/// REOPEN a FRESH PgStore over the same instance/database: the reopened engine
/// reads the persisted head + rows, applies NO re-seed (no collision on the
/// seeded account), and a further mutation continues at head + 1.
#[test]
fn reopen_over_pg_reads_head_and_continues_without_reseeding() {
    let handle = support::acquire();
    let mut factory = handle.factory("reopen");
    let instance = InstanceId::new("ledger-reopen-pg");
    let _guard = SchemaGuard::new(&factory, instance.clone());

    // First boot: create a fresh schema, genesis-seed account 001, open 002.
    let store = factory.create(instance.clone()).expect("create schema");
    let mut a = Engine::load(store, LEDGER_V1, &mut generator()).expect("genesis load");
    open_account(&mut a, "002");
    let head_before = a.head().expect("head A");
    assert!(head_before > CommitSeq::GENESIS, "the commits advanced the head past genesis");
    assert_eq!(
        accounts(&a),
        vec![("001".to_owned(), "100".to_owned()), ("002".to_owned(), "0".to_owned())],
        "engine A holds the seeded and the opened account",
    );

    // The process ends: drop the engine (closing the durable writer).
    drop(a);

    // Restart: a FRESH PgStore over the SAME durable schema, reopened by the engine
    // at its persisted head — no genesis, no re-seed.
    let reopened = factory.reopen(instance.clone()).expect("reopen the durable schema");
    let mut b = Engine::reopen_with_hosts(reopened, LEDGER_V1, &mut generator(), Registry::new())
        .expect("reopen over the populated PostgreSQL store");

    assert_eq!(b.head().expect("head B"), head_before, "the reopened head is the persisted head, not genesis");
    assert_eq!(
        accounts(&b),
        vec![("001".to_owned(), "100".to_owned()), ("002".to_owned(), "0".to_owned())],
        "the reopened engine reads the durably committed rows",
    );

    // A further mutation commits at head + 1 — the durable store continued.
    open_account(&mut b, "003");
    assert_eq!(b.head().expect("head B after"), head_before.next(), "the mutation commits at head + 1");
    assert_eq!(
        accounts(&b),
        vec![
            ("001".to_owned(), "100".to_owned()),
            ("002".to_owned(), "0".to_owned()),
            ("003".to_owned(), "0".to_owned()),
        ],
        "the new account joins the reopened durable state",
    );
    drop(b);

    // The unified boot path picks reopen for the now-populated schema too.
    let reopened = factory.reopen(instance.clone()).expect("reopen the durable schema again");
    let c = Engine::open_with_hosts(reopened, LEDGER_V1, &mut generator(), Registry::new())
        .expect("open_with_hosts reopens the existing schema");
    assert_eq!(c.head().expect("head C"), head_before.next(), "open_with_hosts continued from the persisted head");
}

/// Reopening a durable schema with a definition whose package version differs
/// from the installed one is a §20 migration, not a reopen: it fails loudly and
/// never re-seeds or attaches the wrong shape.
#[test]
fn reopen_over_pg_with_mismatched_version_is_a_loud_error() {
    let handle = support::acquire();
    let mut factory = handle.factory("reopen_mismatch");
    let instance = InstanceId::new("ledger-mismatch-pg");
    let _guard = SchemaGuard::new(&factory, instance.clone());

    let store = factory.create(instance.clone()).expect("create schema");
    let a = Engine::load(store, LEDGER_V1, &mut generator()).expect("genesis load");
    drop(a);

    let reopened = factory.reopen(instance.clone()).expect("reopen the durable schema");
    let Err(error) = Engine::reopen_with_hosts(reopened, LEDGER_V2, &mut generator(), Registry::new())
    else {
        panic!("a version change must not reopen the durable schema");
    };
    match error {
        EngineError::Mismatch(message) => {
            assert!(message.contains("example.ledger@1.0.0"), "names the installed version: {message}");
            assert!(message.contains("example.ledger@2.0.0"), "names the supplied version: {message}");
        }
        other => panic!("expected EngineError::Mismatch, got {other:?}"),
    }
}
