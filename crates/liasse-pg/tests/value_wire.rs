//! End-to-end round-trip of the self-describing value/address codec through
//! PostgreSQL.
//!
//! The shared `contract_tests` battery exercises only `int` keys and `text`
//! payloads, so on its own it never proves the schema-free wire codec preserves
//! the *other* seventeen [`Value`] variants. This test stores one row per variant
//! — under a composite key that itself mixes typed components — commits, then
//! reopens the store from the durable tables and asserts every payload decodes
//! back equal. The expected values are built independently here, so a pass means
//! the codec is a true inverse, not that the store agrees with itself.
//!
//! Like the conformance suite it resolves the test DSN through [`support`]
//! (env override, default socket, or a bootstrapped disposable cluster) and
//! fails with an actionable message if none is reachable — it never silently
//! passes. Its throwaway schema is dropped through a [`support::SchemaGuard`],
//! so cleanup happens even if an assertion panics.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, RowAddress, StoreFactory, Transition,
};
use liasse_value::{
    Bytes, CalendarPeriodBuilder, Decimal, Duration, EnumType, Integer, Json, Period, Precision,
    Ref, Struct, Text, Timestamp, Uuid, Value,
};

/// One value of every [`Value`] variant, built independently of the codec.
fn every_variant() -> Vec<Value> {
    let calendar = {
        let mut builder = CalendarPeriodBuilder {
            years: 1,
            months: 2,
            days: 3,
            zone: Some("Europe/Paris".to_owned()),
            ..Default::default()
        };
        builder.set_overflow("reject").unwrap();
        builder.build().unwrap()
    };
    vec![
        Value::Text(Text::new("héllo")),
        Value::Bool(true),
        Value::Int(Integer::from(-42i64)),
        Value::Decimal(Decimal::parse("3.14159").unwrap()),
        Value::Bytes(Bytes::new(vec![1u8, 2, 255])),
        Value::Uuid(Uuid::parse("00112233-4455-6677-8899-aabbccddeeff").unwrap()),
        Value::Timestamp(Timestamp::new(1_700_000_000_000i128, Precision::Millis)),
        Value::Duration(Duration::from_nanos(1_500_000_000)),
        Value::Period(Box::new(Period::Fixed(Duration::from_nanos(9)))),
        Value::Period(Box::new(Period::Calendar(calendar))),
        Value::Json(Json::from_wire(&serde_json::json!({"a": [1, 2, null], "b": "x"})).unwrap()),
        Value::Enum(
            EnumType::new(["red".to_owned(), "green".to_owned(), "blue".to_owned()])
                .unwrap()
                .parse("green")
                .unwrap(),
        ),
        Value::Ref(Ref::scalar(Value::Int(Integer::from(7i64)))),
        Value::Ref(Ref::composite(vec![
            Value::Int(Integer::from(1i64)),
            Value::Text(Text::new("k")),
        ])),
        Value::Struct(Struct::new([
            (Text::new("x"), Value::Bool(false)),
            (Text::new("y"), Value::Int(Integer::from(9i64))),
        ])),
        Value::Set([Value::Int(Integer::from(3i64)), Value::Int(Integer::from(1i64))].into_iter().collect()),
        Value::Map([(Value::Text(Text::new("k")), Value::Int(Integer::from(5i64)))].into_iter().collect()),
        Value::None,
    ]
}

#[test]
fn every_value_variant_round_trips_through_postgres() {
    let handle = support::acquire();
    let mut factory = handle.factory("wire");
    let instance = InstanceId::new("codec-instance");
    let _schema = support::SchemaGuard::new(&factory, instance.clone());
    let collection = CollectionPath::top(NameSegment::new("v"));
    let variants = every_variant();

    {
        let mut store = factory.create(instance.clone()).expect("create");
        let mut txn = store.begin();
        for (index, value) in variants.iter().enumerate() {
            // A composite key mixing an ascending int with a text component so
            // the address codec is exercised alongside the value codec, and scan
            // order is the insertion order.
            let key = KeyValue::composite(
                Value::Int(Integer::from(i64::try_from(index).unwrap())),
                [Value::Text(Text::new("row"))],
            );
            let address = RowAddress::root(AddressStep::new(NameSegment::new("v"), key));
            txn.insert(address, value.clone()).expect("insert");
        }
        txn.commit().expect("commit");
    }

    // A fresh open rebuilds the projection purely from the durable tables.
    let reopened = factory.reopen(instance.clone()).expect("reopen");
    let recovered: Vec<Value> = reopened
        .scan(&collection)
        .expect("scan")
        .into_iter()
        .map(|(_, row)| row.value().clone())
        .collect();

    assert_eq!(recovered, variants, "every variant decodes back equal");
    // `_schema` drops the throwaway schema on scope exit (and on a panic).
}
