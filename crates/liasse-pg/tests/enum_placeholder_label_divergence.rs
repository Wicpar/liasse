//! Red-team probe: both store backends implement the one `liasse-store` contract
//! and MUST produce identical observable results for any valid [`Value`]
//! (SPEC-ISSUES item 32: a backend disagreement is always a fix). An `enum`
//! label is a wire string (Annex A.1); `U+0000` (NUL) is a valid Unicode scalar,
//! so `Value::Enum` carrying the label `"\u{0}#0"` is a well-formed Liasse value
//! the in-memory reference persists and round-trips verbatim.
//!
//! The PostgreSQL backend is schema-free, so `value_codec::decode_enum` must
//! reconstruct an `EnumValue` from the stored `(ordinal, label)` pair alone. It
//! does so directly through `EnumValue::from_parts` — an `EnumValue` *is* that
//! pair. A regression once rebuilt a synthetic `EnumType` whose first `ordinal`
//! labels were placeholders `format!("\u{0}#{i}")` and whose last was the real
//! label, then re-`parse`d it; when the real label equalled a placeholder,
//! `EnumType::new` rejected the duplicate and the whole projection load failed,
//! so the instance could never be reopened even though its `jsonb`-stored bytes
//! were intact (the NUL survives storage via `jsonb_text`).
//!
//! This gate pins memory-vs-pg agreement for exactly that value: memory commits
//! and reads it back, and pg must rebuild the identical durable state on reopen.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress,
    StoreFactory, StoredRow, Transition,
};
use liasse_value::{EnumType, Integer, Value};

/// A well-formed `Value::Enum` whose ordinal-1 label is exactly the string the
/// pg store uses as its ordinal-0 reconstruction placeholder (`\u{0}#0`).
fn enum_with_placeholder_label() -> Value {
    let declaration =
        EnumType::new(["a".to_owned(), "\u{0}#0".to_owned()]).expect("valid enum declaration");
    Value::Enum(declaration.parse("\u{0}#0").expect("declared label parses"))
}

fn addr() -> RowAddress {
    RowAddress::root(AddressStep::new(
        NameSegment::new("items"),
        KeyValue::single(Value::Int(Integer::from(1i64))),
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
fn enum_placeholder_label_survives_pg_reopen() {
    let value = enum_with_placeholder_label();

    // Memory reference: the value commits and reads back equal.
    let mut mem_factory = MemoryStoreFactory::new();
    let mut mem = mem_factory.create(InstanceId::new("mem")).expect("create mem");
    {
        let mut txn = mem.begin();
        txn.insert(addr(), value.clone()).expect("mem insert");
        txn.commit().expect("mem commit");
    }
    let mem_rows = scan_values(&mem);
    assert_eq!(mem_rows.len(), 1, "the reference holds the enum row");
    let (_, stored) = mem_rows.first().expect("one enum row");
    assert_eq!(*stored, value, "the reference round-trips the enum value");

    // PostgreSQL backend: identical op sequence, then a durable reopen.
    let handle = support::acquire();
    let mut pg_factory = handle.factory("enum-placeholder");
    let instance = InstanceId::new("pg");
    let _schema = support::SchemaGuard::new(&pg_factory, instance.clone());
    {
        let mut pg = pg_factory.create(instance.clone()).expect("create pg");
        let mut txn = pg.begin();
        txn.insert(addr(), value.clone()).expect("pg insert");
        // The commit itself succeeds: `jsonb_text` escapes the NUL for storage.
        txn.commit().expect("pg commit");
    }

    // The divergence surfaces on reopen: rebuilding the projection from the durable
    // tables decodes the enum, whose reconstruction placeholder collides with the
    // real label, so the load fails and the instance cannot be reopened.
    let reopened = pg_factory.reopen(instance).unwrap_or_else(|error| {
        panic!(
            "DIVERGENCE: pg cannot reopen an instance holding a valid enum value the \
             in-memory reference round-trips.\nmemory produced: {mem_rows:?}\npg reopen error: {error:?}"
        )
    });
    let pg_rows = scan_values(&reopened);
    assert_eq!(pg_rows, mem_rows, "both backends must agree after a durable reopen");
}
