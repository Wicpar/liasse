//! Both store backends implement the one `liasse-store` contract and must produce
//! identical observable results for any valid [`Value`]. A `text` value is a
//! sequence of Unicode scalar values (Annex A.1), and `U+0000` (NUL) is a valid
//! Unicode scalar value, so `Value::Text("a\0b")` is a well-formed Liasse value
//! that both backends must persist losslessly (the `value_codec` documents itself
//! as lossless).
//!
//! PostgreSQL `jsonb` cannot hold a raw `U+0000`, so a naive codec rejects such a
//! value at commit while the in-memory reference accepts it — a backend-dependent
//! divergence for a valid input (a store-contract violation, tracked as a fix by
//! SPEC-ISSUES item 32). The `jsonb_text` NUL-safe encoding removes that
//! divergence: these tests pin memory-vs-pg agreement on NUL-bearing text in a
//! value payload, in a text key (which drives Annex B scan order), and inside a
//! nested composite value, and check that scan order is identical across
//! backends.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress,
    StoreError, StoreFactory, StoredRow, Transition,
};
use liasse_value::{Integer, Struct, Text, Value};

/// The rows every backend receives: text payloads and text keys carrying `U+0000`
/// in assorted positions, plus a nested composite (`struct`) whose field value
/// carries one. Keys are chosen so their Annex B order is *not* their insertion
/// order, which makes the scan-order assertion meaningful.
fn rows() -> Vec<(Value, Value)> {
    vec![
        (Value::Text(Text::new("m\u{0}z")), Value::Text(Text::new("payload-mz"))),
        (Value::Text(Text::new("a\u{0}b")), Value::Text(Text::new("a\u{0}b"))),
        (Value::Text(Text::new("plain")), Value::Text(Text::new("no nul here"))),
        (
            Value::Int(Integer::from(1i64)),
            Value::Struct(Struct::new([
                (Text::new("note"), Value::Text(Text::new("nested\u{0}nul"))),
                (Text::new("n"), Value::Int(Integer::from(2i64))),
            ])),
        ),
    ]
}

/// Insert every `(key, payload)` row under collection `items`, commit, and read
/// the collection back in scan order — the single op sequence run against both
/// backends.
fn run_sequence<F: StoreFactory>(
    factory: &mut F,
    instance: InstanceId,
) -> Result<Vec<(RowAddress, Value)>, StoreError> {
    let collection = CollectionPath::top(NameSegment::new("items"));
    let mut store = factory.create(instance)?;
    let mut txn = store.begin();
    for (key, payload) in rows() {
        let address =
            RowAddress::root(AddressStep::new(NameSegment::new("items"), KeyValue::single(key)));
        txn.insert(address, payload)?;
    }
    txn.commit()?;
    Ok(store
        .scan(&collection)?
        .into_iter()
        .map(|(a, r): (RowAddress, StoredRow)| (a, r.value().clone()))
        .collect())
}

#[test]
fn nul_text_persists_identically_across_backends() {
    // Memory reference: the valid values persist and scan in Annex B order.
    let mut mem = MemoryStoreFactory::new();
    let mem_rows = run_sequence(&mut mem, InstanceId::new("mem-instance"))
        .expect("memory store must persist NUL-bearing text values");

    // PostgreSQL backend: the identical sequence must commit and read back equal.
    let handle = support::acquire();
    let mut pg = handle.factory("nul");
    let instance = InstanceId::new("pg-instance");
    let _schema = support::SchemaGuard::new(&pg, instance.clone());

    let pg_rows = run_sequence(&mut pg, instance).unwrap_or_else(|error| {
        panic!(
            "DIVERGENCE: PostgreSQL rejected valid NUL-bearing text that the memory \
             reference accepted: {error:?}\nmemory produced: {mem_rows:?}"
        )
    });

    assert_eq!(
        pg_rows, mem_rows,
        "both backends must agree on the committed state and scan order for valid values"
    );
    // The scan-order agreement above is only meaningful if the NUL-bearing text
    // keys actually reorder the rows; assert the observed order is the Annex B
    // key order the memory reference computes, not insertion order.
    let keys: Vec<Value> = pg_rows
        .iter()
        .map(|(address, _)| {
            address.steps().next().expect("one step").key().components().next().expect("one key").clone()
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "scan is in Annex B key order across the NUL boundary");
}

/// A NUL-bearing text value survives a durable reopen: it is rebuilt from the
/// jsonb tables byte-for-byte, proving the NUL-safe encoding is a true inverse and
/// not an artefact of the live in-memory projection.
#[test]
fn nul_text_survives_pg_reopen() {
    let handle = support::acquire();
    let mut factory = handle.factory("nul-reopen");
    let instance = InstanceId::new("reopen-instance");
    let _schema = support::SchemaGuard::new(&factory, instance.clone());
    let collection = CollectionPath::top(NameSegment::new("items"));

    let expected = run_sequence(&mut factory, instance.clone()).expect("commit NUL-bearing rows");

    let reopened = factory.reopen(instance).expect("reopen");
    let recovered: Vec<(RowAddress, Value)> = reopened
        .scan(&collection)
        .expect("scan")
        .into_iter()
        .map(|(a, r)| (a, r.value().clone()))
        .collect();

    assert_eq!(recovered, expected, "NUL-bearing rows rebuild identically from durable tables");
}
