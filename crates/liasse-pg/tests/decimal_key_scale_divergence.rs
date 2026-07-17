//! Both store backends implement the one `liasse-store` contract and must
//! produce identical observable results (SPEC-ISSUES item 32: a backend
//! disagreement is always a fix). Annex B.1 pins `decimal` order as "mathematical
//! decimal order; numerically equal canonical values compare equal", so `1.0` and
//! `1.00` are the *same* key value — one collection key, one durable row. The
//! in-memory reference keeps a single row (its `RowAddress` `Ord` is Annex B, so a
//! scale-variant key resolves to the same slot).
//!
//! PostgreSQL keys the `rows` table by a JSON rendering of the address whose
//! decimal component used to be the scale-preserving canonical text (`1.0` vs
//! `1.00`) — strictly finer than Annex B identity. A committed update or delete
//! addressed with `1.00` then failed to match the row inserted at `1.0`: the live
//! session agreed with memory (staging keys on the Annex B `RowAddress`), but the
//! durable `rows` statement matched zero rows, so the mutation was lost on reopen.
//! These tests pin memory-vs-pg agreement across a durable reopen for a
//! scale-variant update and delete; the address key now collapses Annex-B-equal
//! decimals to one identity.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress,
    StoreFactory, StoredRow, Transition,
};
use liasse_value::{Decimal, Text, Value};

fn decimal_address(scale_text: &str) -> RowAddress {
    RowAddress::root(AddressStep::new(
        NameSegment::new("items"),
        KeyValue::single(Value::Decimal(Decimal::parse(scale_text).expect("valid decimal"))),
    ))
}

fn scan_values<S: InstanceStore>(store: &S) -> Vec<(RowAddress, Value)> {
    store
        .scan(&CollectionPath::top(NameSegment::new("items")))
        .expect("scan")
        .into_iter()
        .map(|(a, r): (RowAddress, StoredRow)| (a, r.value().clone()))
        .collect()
}

#[test]
fn memory_reference_treats_scale_variant_keys_as_one_row() {
    let mut factory = MemoryStoreFactory::new();
    let mut store = factory.create(InstanceId::new("mem")).expect("create");

    let mut txn = store.begin();
    txn.insert(decimal_address("1.0"), Value::Text(Text::new("original"))).expect("insert 1.0");
    txn.commit().expect("commit insert");

    let mut txn = store.begin();
    txn.update(&decimal_address("1.00"), Value::Text(Text::new("updated"))).expect("update 1.00");
    txn.commit().expect("commit update");

    let rows = scan_values(&store);
    assert_eq!(rows.len(), 1, "scale variants are one row, not two");
    let (_, value) = rows.first().expect("one row");
    assert_eq!(*value, Value::Text(Text::new("updated")), "the update took effect");
}

#[test]
fn committed_update_by_scale_variant_key_survives_pg_reopen() {
    let mut mem_factory = MemoryStoreFactory::new();
    let mut mem = mem_factory.create(InstanceId::new("mem")).expect("create mem");
    {
        let mut txn = mem.begin();
        txn.insert(decimal_address("1.0"), Value::Text(Text::new("original"))).expect("mem insert");
        txn.commit().expect("mem commit insert");
        let mut txn = mem.begin();
        txn.update(&decimal_address("1.00"), Value::Text(Text::new("updated"))).expect("mem update");
        txn.commit().expect("mem commit update");
    }
    let mem_rows = scan_values(&mem);

    let handle = support::acquire();
    let mut pg_factory = handle.factory("dec-scale-update");
    let instance = InstanceId::new("pg");
    let _schema = support::SchemaGuard::new(&pg_factory, instance.clone());

    {
        let mut pg = pg_factory.create(instance.clone()).expect("create pg");
        let mut txn = pg.begin();
        txn.insert(decimal_address("1.0"), Value::Text(Text::new("original"))).expect("pg insert");
        txn.commit().expect("pg commit insert");
        let mut txn = pg.begin();
        txn.update(&decimal_address("1.00"), Value::Text(Text::new("updated"))).expect("pg update");
        txn.commit().expect("pg commit update");
    }

    let reopened = pg_factory.reopen(instance).expect("reopen");
    let pg_rows = scan_values(&reopened);

    assert_eq!(
        pg_rows, mem_rows,
        "both backends must agree after a durable reopen; the update addressed by \
         the scale-variant decimal key must survive"
    );
}

#[test]
fn committed_delete_by_scale_variant_key_survives_pg_reopen() {
    let mut mem_factory = MemoryStoreFactory::new();
    let mut mem = mem_factory.create(InstanceId::new("mem")).expect("create mem");
    {
        let mut txn = mem.begin();
        txn.insert(decimal_address("1.0"), Value::Text(Text::new("v"))).expect("mem insert");
        txn.commit().expect("mem commit insert");
        let mut txn = mem.begin();
        txn.delete(&decimal_address("1.00")).expect("mem delete");
        txn.commit().expect("mem commit delete");
    }
    let mem_rows = scan_values(&mem);
    assert!(mem_rows.is_empty(), "reference: the delete emptied the collection");

    let handle = support::acquire();
    let mut pg_factory = handle.factory("dec-scale-delete");
    let instance = InstanceId::new("pg");
    let _schema = support::SchemaGuard::new(&pg_factory, instance.clone());

    {
        let mut pg = pg_factory.create(instance.clone()).expect("create pg");
        let mut txn = pg.begin();
        txn.insert(decimal_address("1.0"), Value::Text(Text::new("v"))).expect("pg insert");
        txn.commit().expect("pg commit insert");
        let mut txn = pg.begin();
        txn.delete(&decimal_address("1.00")).expect("pg delete");
        txn.commit().expect("pg commit delete");
    }

    let reopened = pg_factory.reopen(instance).expect("reopen");
    let pg_rows = scan_values(&reopened);

    assert_eq!(
        pg_rows, mem_rows,
        "both backends must agree after a durable reopen; the delete addressed by \
         the scale-variant decimal key must not resurrect the row"
    );
}
