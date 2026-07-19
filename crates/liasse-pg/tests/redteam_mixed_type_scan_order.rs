//! RED TEAM — a single collection populated with keys of MANY different Annex-A
//! types, scanned end-to-end, to prove the pg `ORDER BY key_enc` (bytea memcmp)
//! reproduces the reference's cross-type `Value::Ord` (Annex B) — not just at the
//! codec level the `key_enc_proptest` gates, but through the real SQL scan and the
//! `key_wire` address reconstruction (SPEC-ISSUES item 32: a backend disagreement
//! is always a fix).
//!
//! The store is semantics-free, so one collection may legitimately hold rows keyed
//! by `bool`, `int` (incl. `i64::MIN`/`MAX`), scale-variant `decimal`, edge `text`
//! (empty, interior `U+0000`), edge `bytes` (empty, interior `0x00`), `uuid`,
//! `none`, and a mixed composite. The `key_enc` rank byte orders cross-type at byte
//! 0; if any rank were mis-assigned relative to `Value::Ord`, the pg scan would come
//! back in a different order than the reference — an observable, HIGH divergence.
//! Checked live and across a durable reopen.
//!
//! Resolves the DSN through [`support`] and drops its throwaway schema through a
//! [`support::SchemaGuard`] even on a panic.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress,
    StoreFactory, StoredRow, Transition,
};
use liasse_value::{Bytes, Decimal, Integer, Text, Uuid, Value};

fn text(payload: &str) -> Value {
    Value::Text(Text::new(payload))
}

/// A spread of key values across types, insertion order deliberately NOT sorted so
/// the scan must impose Annex-B order itself.
fn wild_keys() -> Vec<KeyValue> {
    vec![
        KeyValue::single(Value::Bool(true)),
        KeyValue::single(Value::Int(Integer::from(i64::MAX))),
        KeyValue::single(text("z-last")),
        KeyValue::single(Value::None),
        KeyValue::single(Value::Bool(false)),
        KeyValue::single(Value::Int(Integer::from(i64::MIN))),
        KeyValue::single(Value::Int(Integer::from(0))),
        KeyValue::single(Value::Decimal(Decimal::parse("1.500").expect("dec"))),
        KeyValue::single(Value::Decimal(Decimal::parse("-2.25").expect("dec"))),
        KeyValue::single(text("")),
        KeyValue::single(text("a\u{0}b")),
        KeyValue::single(Value::Bytes(Bytes::new(Vec::new()))),
        KeyValue::single(Value::Bytes(Bytes::new(vec![0u8, 0u8, 255u8]))),
        KeyValue::single(Value::Bytes(Bytes::new(vec![255u8]))),
        KeyValue::single(Value::Uuid(Uuid::from_bytes([0x11; 16]))),
        KeyValue::single(Value::Uuid(Uuid::from_bytes([0xEE; 16]))),
        KeyValue::single(Value::Int(Integer::from(-1))),
        // Mixed composite keys: (text, int) and (int, none).
        KeyValue::composite(text("k"), [Value::Int(Integer::from(7))]),
        KeyValue::composite(Value::Int(Integer::from(7)), [Value::None]),
    ]
}

fn wild(key: KeyValue) -> RowAddress {
    RowAddress::root(AddressStep::new(NameSegment::new("wild"), key))
}

fn build<S: InstanceStore>(store: &mut S) {
    for (i, key) in wild_keys().into_iter().enumerate() {
        let mut txn = store.begin();
        txn.insert(wild(key), text(&format!("row-{i}"))).expect("insert wild");
        txn.commit().expect("commit wild");
    }
}

fn assert_scan_matches<A: InstanceStore, B: InstanceStore>(memory: &A, pg: &B, label: &str) {
    let collection = CollectionPath::top(NameSegment::new("wild"));
    let m: Vec<(RowAddress, StoredRow)> = memory.scan(&collection).expect("mem scan");
    let p: Vec<(RowAddress, StoredRow)> = pg.scan(&collection).expect("pg scan");
    assert_eq!(
        m.len(),
        wild_keys().len(),
        "{label}: every distinct-typed key is one row, got {}",
        m.len()
    );
    assert_eq!(
        m, p,
        "{label}: mixed-type scan ORDER/membership divergence — the pg ORDER BY key_enc must \
         reproduce the reference cross-type Value::Ord.\n memory={m:?}\n pg={p:?}"
    );
}

#[test]
fn mixed_type_scan_order_matches_memory() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("wildscan");
    let instance = InstanceId::new("mixed-type-scan-order");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");
    build(&mut memory);
    build(&mut pg);

    assert_scan_matches(&memory, &pg, "live");

    // Point reads for every key must agree too (the chained InitPlan resolves each
    // typed key_enc identically to the reference's typed address).
    for key in wild_keys() {
        let a = wild(key);
        assert_eq!(
            memory.row(&a).expect("mem row"),
            pg.row(&a).expect("pg row"),
            "point read divergence at {}",
            a.render()
        );
    }

    // Durable reopen: order/round-trip preserved off the tree.
    let reopened = pg_factory.reopen(instance.clone()).expect("reopen pg store");
    assert_scan_matches(&memory, &reopened, "reopened");
    // `_guard` drops the throwaway schema on scope exit (and on a panic).
}
