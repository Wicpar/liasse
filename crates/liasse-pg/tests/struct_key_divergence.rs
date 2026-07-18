//! A struct-typed collection key (SPEC.md A.8: "structs composed solely of
//! key-eligible required fields") must behave identically across both store
//! backends — the in-memory reference and PostgreSQL — through insert, scan,
//! rekey, and a durable reopen (SPEC-ISSUES item 32: a backend disagreement is
//! always a fix, never a skip).
//!
//! The key is a `Value::Struct { x, y }`. Annex B.4 orders a struct by its fields
//! in canonical field-name (text) order, so `x` is the primary sort component and
//! `y` the secondary — NOT the members' declaration order. Both backends must key,
//! order, rekey, and reload a struct-valued key by that rule, and their scans must
//! agree row-for-row after a durable reopen. Every expected order below is derived
//! from B.4, not from either backend's own answer.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress,
    StoreFactory, StoredRow, Transition,
};
use liasse_value::{Integer, Struct, Text, Value};

fn int(n: i64) -> Value {
    Value::Int(Integer::parse(&n.to_string()).expect("valid int"))
}

/// The struct key `{ x, y }` at the `cells` collection.
fn cell(x: i64, y: i64) -> RowAddress {
    let key = Value::Struct(Struct::new([(Text::new("x"), int(x)), (Text::new("y"), int(y))]));
    RowAddress::root(AddressStep::new(NameSegment::new("cells"), KeyValue::single(key)))
}

/// The `(address, payload)` pairs of `cells`, in the store's Annex-B scan order.
fn scan_pairs<S: InstanceStore>(store: &S) -> Vec<(RowAddress, Value)> {
    store
        .scan(&CollectionPath::top(NameSegment::new("cells")))
        .expect("scan")
        .into_iter()
        .map(|(a, r): (RowAddress, StoredRow)| (a, r.value().clone()))
        .collect()
}

/// The payload tags of a scan, in order — the externally-checkable B.4 sequence.
fn payload_tags(pairs: &[(RowAddress, Value)]) -> Vec<Value> {
    pairs.iter().map(|(_, v)| v.clone()).collect()
}

fn tag(t: &str) -> Value {
    Value::Text(Text::new(t))
}

/// Insert four struct-keyed rows (scrambled), commit; then rekey (2,1) -> (3,3)
/// preserving its payload, commit. The identical sequence runs on each backend.
fn drive<S: InstanceStore>(store: &mut S) {
    let mut txn = store.begin();
    txn.insert(cell(2, 1), tag("a")).expect("insert (2,1)");
    txn.insert(cell(1, 5), tag("b")).expect("insert (1,5)");
    txn.insert(cell(1, 2), tag("c")).expect("insert (1,2)");
    txn.insert(cell(2, 0), tag("d")).expect("insert (2,0)");
    txn.commit().expect("commit inserts");

    let mut txn = store.begin();
    txn.rekey(&cell(2, 1), cell(3, 3), tag("a")).expect("rekey (2,1)->(3,3)");
    txn.commit().expect("commit rekey");
}

/// The B.4 order after the sequence: compare `x` then `y` ascending, with (2,1)
/// moved to (3,3). Payload tags in that order are c, b, d, a.
fn expected_order() -> Vec<(RowAddress, Value)> {
    vec![
        (cell(1, 2), tag("c")),
        (cell(1, 5), tag("b")),
        (cell(2, 0), tag("d")),
        (cell(3, 3), tag("a")),
    ]
}

#[test]
fn memory_reference_orders_and_rekeys_a_struct_key_by_field_name() {
    let mut factory = MemoryStoreFactory::new();
    let mut store = factory.create(InstanceId::new("mem")).expect("create");
    drive(&mut store);

    let pairs = scan_pairs(&store);
    assert_eq!(
        pairs,
        expected_order(),
        "the in-memory reference orders a struct key by field-name (B.4): x then y, \
         with the rekeyed row now at (3,3)"
    );
    // Spell out the ordering claim independently: the tag sequence is the B.4 order.
    assert_eq!(payload_tags(&pairs), vec![tag("c"), tag("b"), tag("d"), tag("a")]);
}

#[test]
fn struct_key_insert_rekey_and_scan_agree_across_a_pg_reopen() {
    // Reference: drive the sequence through the in-memory store.
    let mut mem_factory = MemoryStoreFactory::new();
    let mut mem = mem_factory.create(InstanceId::new("mem")).expect("create mem");
    drive(&mut mem);
    let mem_rows = scan_pairs(&mem);

    // PostgreSQL: the identical sequence, then a durable reopen — the state must
    // survive being written, dropped, and reloaded.
    let handle = support::acquire();
    let mut pg_factory = handle.factory("struct-key");
    let instance = InstanceId::new("pg");
    let _schema = support::SchemaGuard::new(&pg_factory, instance.clone());

    {
        let mut pg = pg_factory.create(instance.clone()).expect("create pg");
        drive(&mut pg);
    }
    let reopened = pg_factory.reopen(instance).expect("reopen");
    let pg_rows = scan_pairs(&reopened);

    assert_eq!(
        pg_rows, mem_rows,
        "both backends must agree row-for-row after a durable reopen: a struct-valued \
         key inserts, orders by B.4 field-name order, rekeys, and reloads identically"
    );
    // And that shared state is exactly the B.4 order derived from the spec, so the
    // agreement is not two backends sharing the same wrong answer.
    assert_eq!(pg_rows, expected_order(), "the durable pg state is the spec's B.4 order");
}
