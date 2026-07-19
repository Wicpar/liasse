//! The eval-wire codec round-trips every `Value` class losslessly (§7.4).
//!
//! `encode ∘ decode = id` is the pinned gate: a hoisted env [`Cell`] carrying any
//! canonical value survives a `postcard` round-trip byte-for-byte in meaning. The
//! oracle is `Value`'s own Annex-B equality — externally deducible, per AGENTS.md —
//! and the strategy spans every scalar class (NUL text, mixed-scale decimals,
//! boundary ints, every timestamp precision, `none`) plus nested
//! struct/set/map/ref/composite so the recursive codec is exercised whole.

#![allow(clippy::unwrap_used, clippy::panic)]

use liasse_expr::wire::{env_from_wire, env_to_wire};
use liasse_expr::Cell;
use liasse_value::bigdecimal::BigDecimal;
use liasse_value::num_bigint::BigInt;
use liasse_value::{
    Bytes, CalendarPeriodBuilder, Date, Decimal, Duration, EnumValue, Integer, Json, MediaType,
    Period, Precision, Ref, Sha512, Struct, Text, Timestamp, Uuid, Value,
};
use proptest::prelude::*;

fn roundtrip(value: &Value) -> Value {
    let wire = env_to_wire(&[("x".to_owned(), Cell::scalar(value.clone()))]).unwrap();
    let decoded = env_from_wire(&wire).unwrap();
    match decoded.into_iter().next() {
        Some((_, Cell::Scalar(value))) => value,
        other => panic!("round-trip did not preserve a scalar cell: {other:?}"),
    }
}

fn pow10(exponent: u32) -> BigInt {
    (0..exponent).fold(BigInt::from(1i64), |acc, _| acc * BigInt::from(10i64))
}

fn small_string() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![Just('\u{0}'), Just('a'), Just('é'), Just('\u{10348}'), any::<char>()],
        0..5,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn leaf_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Int(Integer::from(BigInt::from(n) * BigInt::from(n)))),
        (-100_000i64..100_000, -6i64..8, 0u32..4).prop_map(|(m, s, pad)| {
            Value::Decimal(Decimal::from_big_decimal(BigDecimal::new(
                BigInt::from(m) * pow10(pad),
                s + i64::from(pad),
            )))
        }),
        small_string().prop_map(|t| Value::Text(Text::new(t))),
        prop::collection::vec(any::<u8>(), 0..6).prop_map(|b| Value::Bytes(Bytes::new(b))),
        any::<[u8; 16]>().prop_map(|b| Value::Uuid(Uuid::from_bytes(b))),
        (1i32..9999, 1u32..=12, 1u32..=28)
            .prop_map(|(y, m, d)| Date::parse(&format!("{y:04}-{m:02}-{d:02}")).unwrap())
            .prop_map(Value::Date),
        (any::<i64>(), timestamp_precision())
            .prop_map(|(c, p)| Value::Timestamp(Timestamp::new(i128::from(c), p))),
        any::<i64>().prop_map(|n| Value::Duration(Duration::from_nanos(i128::from(n)))),
        any::<i64>().prop_map(|n| Value::Period(Box::new(Period::Fixed(Duration::from_nanos(i128::from(n)))))),
        (0u32..4, small_string()).prop_map(|(o, l)| Value::Enum(EnumValue::from_parts(o, l))),
        small_string().prop_map(|s| Value::Json(Json::from_wire(&serde_json::Value::String(s)).unwrap())),
        Just(Value::None),
    ]
}

fn timestamp_precision() -> impl Strategy<Value = Precision> {
    prop_oneof![
        Just(Precision::Seconds),
        Just(Precision::Millis),
        Just(Precision::Micros),
        Just(Precision::Nanos),
    ]
}

fn value_strategy() -> impl Strategy<Value = Value> {
    leaf_value().prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::btree_map(small_string(), inner.clone(), 0..4).prop_map(|f| {
                Value::Struct(Struct::new(f.into_iter().map(|(n, v)| (Text::new(n), v))))
            }),
            prop::collection::btree_set(inner.clone(), 0..4).prop_map(Value::Set),
            prop::collection::btree_map(inner.clone(), inner.clone(), 0..4).prop_map(Value::Map),
            inner.clone().prop_map(|v| Value::Ref(Ref::scalar(v))),
            prop::collection::vec(inner.clone(), 1..3).prop_map(|c| Value::Ref(Ref::composite(c))),
            prop::collection::vec(inner, 2..4).prop_map(Value::Composite),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 4000, ..ProptestConfig::default() })]

    #[test]
    fn every_value_class_round_trips(value in value_strategy()) {
        prop_assert_eq!(roundtrip(&value), value);
    }
}

#[test]
fn calendar_period_round_trips() {
    let mut builder = CalendarPeriodBuilder {
        years: 1,
        months: 2,
        weeks: 0,
        days: 15,
        time: Duration::from_nanos(3_600_000_000_000),
        zone: Some("Europe/Paris".to_owned()),
        ..CalendarPeriodBuilder::default()
    };
    builder.set_overflow("clamp").unwrap();
    builder.set_ambiguous("earlier").unwrap();
    builder.set_missing("forward").unwrap();
    let value = Value::Period(Box::new(Period::Calendar(builder.build().unwrap())));
    assert_eq!(roundtrip(&value), value);
}

#[test]
fn blob_descriptor_round_trips() {
    let sha = Sha512::parse(&"07".repeat(64)).unwrap();
    let value = Value::Blob(Box::new(liasse_value::BlobDescriptor::new(
        sha,
        42,
        MediaType::new("text/plain".to_owned()),
        Some("note.txt".to_owned()),
    )));
    assert_eq!(roundtrip(&value), value);
}
