//! Both store backends implement the one `liasse-store` contract and MUST produce
//! identical observable results for any valid input (SPEC §22/§23; SPEC-ISSUES
//! item 32: a memory-vs-pg disagreement is always a fix). `LineageId`, `PointId`,
//! `TransactionId` and `InstanceId` are unvalidated opaque tokens (D.1/D.5), and a
//! definition's source is arbitrary A.1 text; `U+0000` (NUL) is a valid Unicode
//! scalar, so each may carry one, and the in-memory reference records it verbatim.
//!
//! PostgreSQL rejects a raw `U+0000` in a `text`/`varchar` value (`ERROR: invalid
//! byte sequence for encoding "UTF8": 0x00`, SQLSTATE 22021). The round-1 NUL fix
//! guarded only the `jsonb` columns; the opaque-token identities live in raw `text`
//! columns (`commit_log.transaction_id`, `history_points.lineage`/`point`,
//! `instance_meta.definition_source`/`instance_id`), so a NUL-bearing identity used
//! to commit on the reference broke the pg commit — a store-contract divergence.
//!
//! These gates pin memory-vs-pg agreement for a NUL (and, for the definition
//! source, a NUL *and* a backslash — the escape's own escape character) in every
//! such column: pg must commit identically to the reference and rebuild the exact
//! durable state on reopen. The NUL-safe `text` encoding is symmetric on write and
//! on the projection rebuild, and its bijection preserves the `(lineage, point)`
//! primary-key equality a history-point lookup depends on.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use liasse_ident::{HistoryPoint, InstanceId, LineageId, NameSegment, PointId, TransactionId};
use liasse_store::{
    AddressStep, CollectionPath, CommitOutcome, CommitSeq, DefinitionText, InstanceStore, KeyValue,
    MemoryStoreFactory, RowAddress, StoreFactory, Transition,
};
use liasse_value::{Integer, Text, Value};

fn addr(n: i64) -> RowAddress {
    RowAddress::root(AddressStep::new(
        NameSegment::new("items"),
        KeyValue::single(Value::Int(Integer::from(n))),
    ))
}

/// The transaction ids of every committed transition, oldest first — the observable
/// projection of the `commit_log.transaction_id` column.
fn transaction_ids<S: InstanceStore>(store: &S) -> Vec<Option<String>> {
    store
        .log_from(CommitSeq::GENESIS)
        .expect("log")
        .iter()
        .map(|t| t.transaction().map(|id| id.as_str().to_owned()))
        .collect()
}

// --------------------------------------------------------------------------
// commit_log.transaction_id
// --------------------------------------------------------------------------
#[test]
fn nul_transaction_id_agrees_across_backends_and_reopen() {
    let tx = || TransactionId::new("tx\u{0}id");

    let mut mem_factory = MemoryStoreFactory::new();
    let mut mem = mem_factory.create(InstanceId::new("mem")).expect("create mem");
    {
        let mut txn = mem.begin();
        txn.insert(addr(1), Value::Text(Text::new("v"))).expect("mem insert");
        txn.set_transaction(tx());
        assert!(matches!(txn.commit().expect("mem commit"), CommitOutcome::Committed(_)));
    }
    let expected = transaction_ids(&mem);
    assert_eq!(expected, vec![Some("tx\u{0}id".to_owned())], "reference records the NUL-bearing id");

    let handle = support::acquire();
    let mut pg_factory = handle.factory("nul-txn");
    let instance = InstanceId::new("pg");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());
    {
        let mut pg = pg_factory.create(instance.clone()).expect("create pg");
        let mut txn = pg.begin();
        txn.insert(addr(1), Value::Text(Text::new("v"))).expect("pg insert");
        txn.set_transaction(tx());
        txn.commit().expect("pg commit must accept a NUL-bearing transaction id");
        assert_eq!(transaction_ids(&pg), expected, "pg live agrees with the reference");
    }
    let reopened = pg_factory.reopen(instance).expect("reopen");
    assert_eq!(transaction_ids(&reopened), expected, "pg rebuilds the id verbatim on reopen");
}

// --------------------------------------------------------------------------
// instance_meta.definition_source  (NUL *and* a backslash: the escape's own char)
// --------------------------------------------------------------------------
#[test]
fn nul_definition_source_agrees_across_backends_and_reopen() {
    let source = "line1\u{0}line2 \\ trailing \\0 literal";
    let def = || DefinitionText::new(source);

    let mut mem_factory = MemoryStoreFactory::new();
    let mut mem = mem_factory.create(InstanceId::new("mem")).expect("create mem");
    {
        let mut txn = mem.begin();
        txn.set_definition(def());
        txn.commit().expect("mem commit");
    }
    let expected = mem.definition().cloned();
    assert_eq!(expected.as_ref().map(DefinitionText::source), Some(source), "reference keeps source");

    let handle = support::acquire();
    let mut pg_factory = handle.factory("nul-def");
    let instance = InstanceId::new("pg");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());
    {
        let mut pg = pg_factory.create(instance.clone()).expect("create pg");
        let mut txn = pg.begin();
        txn.set_definition(def());
        txn.commit().expect("pg commit must accept NUL/backslash definition source");
        assert_eq!(pg.definition().cloned(), expected, "pg live agrees with the reference");
    }
    let reopened = pg_factory.reopen(instance).expect("reopen");
    assert_eq!(reopened.definition().cloned(), expected, "pg rebuilds the source verbatim on reopen");
}

// --------------------------------------------------------------------------
// history_points.lineage / history_points.point
// --------------------------------------------------------------------------
#[test]
fn nul_history_point_agrees_across_backends_and_reopen() {
    let point = || HistoryPoint::new(LineageId::new("lin\u{0}eage"), PointId::new("po\u{0}int"));

    let mut mem_factory = MemoryStoreFactory::new();
    let mut mem = mem_factory.create(InstanceId::new("mem")).expect("create mem");
    let at = {
        let mut txn = mem.begin();
        txn.insert(addr(1), Value::Text(Text::new("v"))).expect("mem insert");
        match txn.commit().expect("mem commit") {
            CommitOutcome::Committed(seq) => seq,
            other => panic!("expected a committed position, got {other:?}"),
        }
    };
    mem.record_point(at, point()).expect("mem record");
    let expected = mem.point_position(&point());
    assert_eq!(expected, Some(at), "reference records the NUL-bearing point");

    let handle = support::acquire();
    let mut pg_factory = handle.factory("nul-point");
    let instance = InstanceId::new("pg");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());
    {
        let mut pg = pg_factory.create(instance.clone()).expect("create pg");
        let pat = {
            let mut txn = pg.begin();
            txn.insert(addr(1), Value::Text(Text::new("v"))).expect("pg insert");
            match txn.commit().expect("pg commit") {
                CommitOutcome::Committed(seq) => seq,
                other => panic!("expected a committed position, got {other:?}"),
            }
        };
        pg.record_point(pat, point()).expect("pg record_point must accept a NUL-bearing point");
        assert_eq!(pg.point_position(&point()), Some(pat), "pg live agrees with the reference");
    }
    let reopened = pg_factory.reopen(instance).expect("reopen");
    assert_eq!(reopened.point_position(&point()), expected, "pg rebuilds the point verbatim on reopen");
}

// --------------------------------------------------------------------------
// instance_meta.instance_id
// --------------------------------------------------------------------------
#[test]
fn nul_instance_id_commits_and_reopens() {
    let items = CollectionPath::top(NameSegment::new("items"));
    let instance = InstanceId::new("pg\u{0}inst");

    // Reference: a NUL-bearing instance identity is a legal opaque token.
    let mut mem_factory = MemoryStoreFactory::new();
    let mut mem = mem_factory.create(instance.clone()).expect("create mem");
    {
        let mut txn = mem.begin();
        txn.insert(addr(1), Value::Text(Text::new("v"))).expect("mem insert");
        txn.commit().expect("mem commit");
    }
    let mem_rows = mem.scan(&items).expect("mem scan");
    assert_eq!(mem.instance().as_str(), "pg\u{0}inst", "reference keeps the instance identity");

    // pg: create seeds `instance_meta.instance_id`; that INSERT must not choke on
    // the NUL, and the instance must reopen and read back identical state.
    let handle = support::acquire();
    let mut pg_factory = handle.factory("nul-inst");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());
    {
        let mut pg = pg_factory.create(instance.clone()).expect("create pg must seed a NUL instance id");
        let mut txn = pg.begin();
        txn.insert(addr(1), Value::Text(Text::new("v"))).expect("pg insert");
        txn.commit().expect("pg commit");
        assert_eq!(pg.instance().as_str(), "pg\u{0}inst", "pg keeps the instance identity");
    }
    let reopened = pg_factory.reopen(instance).expect("reopen a NUL-instance schema");
    assert_eq!(reopened.scan(&items).expect("pg scan"), mem_rows, "state agrees after reopen");
}
