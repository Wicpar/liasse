//! Property gate for the order-preserving `key_enc` codec.
//!
//! The decisive correctness property of the codec is that PostgreSQL's native
//! `bytea` comparison over `encode` reproduces `Value`/`KeyValue`'s Annex-B
//! `Ord`, and collapses Annex-B-equal values to identical bytes:
//!
//! ```text
//! sign(memcmp(encode a, encode b)) == sign(a.cmp(b))
//! a.cmp(b) == Equal            <=>  encode a == encode b
//! ```
//!
//! `Vec<u8>`'s own `Ord` *is* PostgreSQL's default `bytea` order (unsigned
//! `memcmp`, then shorter-first), so comparing the two encodings' `Ordering`
//! against the values' `Ordering` gates the sign equality; because the encoding
//! is order-preserving and prefix-free, that single equality also entails the
//! byte-identical-iff-equal half (Ord-equal ⟺ same `Ordering::Equal` ⟺ equal
//! bytes). The `Value::cmp` oracle is the derived Annex-B order the reference
//! `MemoryStore` sorts its `BTreeMap<RowAddress>` by, so a pass means the durable
//! key order matches the in-memory reference for every generated value.
//!
//! The recursive strategy spans every key-eligible variant, `none`, and nested
//! `struct`/`set`/`map`/`ref`/`enum` (plus `json`/`blob` for total-codec
//! coverage), and is deliberately heavy on negatives, zero, mixed-scale decimals,
//! boundary/huge ints, NUL and non-ASCII text, empty text/bytes, and
//! none-in-composite. Independent `a`/`b` draws make most pairs cross-type.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use liasse_store::KeyValue;
use liasse_value::bigdecimal::BigDecimal;
use liasse_value::num_bigint::BigInt;
use liasse_value::{
    BlobDescriptor, Bytes, Date, Decimal, Duration, EnumValue, Integer, Json, MediaType, Precision,
    Ref, Sha512, Struct, Text, Timestamp, Uuid, Value,
};
use proptest::prelude::*;

use crate::key_enc;

fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    key_enc::encode_value(value, &mut out);
    out
}

/// `10^exponent` as a `BigInt`, without depending on a `pow` inherent method.
fn pow10(exponent: u32) -> BigInt {
    (0..exponent).fold(BigInt::from(1i64), |acc, _| acc * BigInt::from(10i64))
}

/// Short strings drawn from a mix that stresses NUL escaping, multi-byte UTF-8,
/// and emptiness.
fn small_string() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just('\u{0}'),
            Just('a'),
            Just('b'),
            Just('é'),
            Just('\u{10348}'),
            any::<char>(),
        ],
        0..5,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn int_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Int(Integer::from(0i64))),
        Just(Value::Int(Integer::from(i64::MIN))),
        Just(Value::Int(Integer::from(i64::MAX))),
        Just(Value::Int(Integer::from(BigInt::from(i128::MIN)))),
        Just(Value::Int(Integer::from(BigInt::from(i128::MAX)))),
        any::<i64>().prop_map(|n| Value::Int(Integer::from(n))),
        // Products of three i64 reach ~189-bit magnitudes (huge keys), including
        // zero and both signs.
        (any::<i64>(), any::<i64>(), any::<i64>()).prop_map(|(a, b, c)| {
            Value::Int(Integer::from(BigInt::from(a) * BigInt::from(b) * BigInt::from(c)))
        }),
    ]
}

fn decimal_value() -> impl Strategy<Value = Value> {
    (-100_000i64..100_000, -8i64..12, 0u32..4).prop_map(|(mantissa, scale, pad)| {
        // Carry the same numeric value at a larger scale (pad redundant trailing
        // zeros) so scale-variant equal decimals — 1, 1.0, 1.00 — recur and must
        // encode byte-identically.
        let padded = BigInt::from(mantissa) * pow10(pad);
        Value::Decimal(Decimal::from_big_decimal(BigDecimal::new(padded, scale + i64::from(pad))))
    })
}

fn timestamp_value() -> impl Strategy<Value = Value> {
    let precision = prop_oneof![
        Just(Precision::Seconds),
        Just(Precision::Millis),
        Just(Precision::Micros),
        Just(Precision::Nanos),
    ];
    prop_oneof![
        (any::<i64>(), precision.clone())
            .prop_map(|(count, prec)| Value::Timestamp(Timestamp::new(i128::from(count), prec))),
        // Whole-second instants at varied precision, so (1000, ms) and (1, s)
        // collide and must encode identically.
        (-100_000i64..100_000, precision).prop_map(|(whole, prec)| {
            Value::Timestamp(Timestamp::new(i128::from(whole) * prec.ticks_per_second(), prec))
        }),
    ]
}

fn date_value() -> impl Strategy<Value = Value> {
    (1i32..9999, 1u32..=12, 1u32..=28).prop_map(|(year, month, day)| {
        Date::parse(&format!("{year:04}-{month:02}-{day:02}"))
            .map(Value::Date)
            .unwrap_or(Value::None)
    })
}

fn enum_value() -> impl Strategy<Value = Value> {
    let label = prop_oneof![
        Just(String::new()),
        Just("a".to_owned()),
        Just("b".to_owned()),
        Just("\u{0}".to_owned()),
        Just("z".to_owned()),
    ];
    // Small ordinals and a small label set so both the ordinal key and the label
    // tiebreak (B.1 / EnumValue::cmp) get exercised.
    (0u32..4, label).prop_map(|(ordinal, label)| Value::Enum(EnumValue::from_parts(ordinal, label)))
}

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Json::Null),
        any::<bool>().prop_map(Json::Bool),
        (-10_000i64..10_000, -4i64..6, 0u32..4).prop_map(|(mantissa, scale, pad)| {
            Json::Number(BigDecimal::new(BigInt::from(mantissa) * pow10(pad), scale + i64::from(pad)))
        }),
        small_string().prop_map(Json::String),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Json::Array),
            prop::collection::btree_map(small_string(), inner, 0..4).prop_map(Json::Object),
        ]
    })
    .prop_map(Value::Json)
}

fn blob_value() -> impl Strategy<Value = Value> {
    (
        any::<[u8; 64]>(),
        any::<u64>(),
        small_string(),
        prop::option::of(small_string()),
    )
        .prop_map(|(digest, byte_count, media, name)| {
            let sha = Sha512::parse(&data_encoding::HEXLOWER.encode(&digest))
                .expect("64-byte digest renders to valid lowercase hex");
            Value::Blob(Box::new(BlobDescriptor::new(
                sha,
                byte_count,
                MediaType::new(media),
                name,
            )))
        })
}

fn leaf_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<bool>().prop_map(Value::Bool),
        int_value(),
        decimal_value(),
        small_string().prop_map(|text| Value::Text(Text::new(text))),
        prop::collection::vec(prop_oneof![Just(0u8), Just(0xFFu8), any::<u8>()], 0..6)
            .prop_map(|bytes| Value::Bytes(Bytes::new(bytes))),
        any::<[u8; 16]>().prop_map(|bytes| Value::Uuid(Uuid::from_bytes(bytes))),
        date_value(),
        timestamp_value(),
        any::<i64>().prop_map(|nanos| Value::Duration(Duration::from_nanos(i128::from(nanos)))),
        enum_value(),
        json_value(),
        blob_value(),
        Just(Value::None),
    ]
}

fn value_strategy() -> impl Strategy<Value = Value> {
    leaf_value().prop_recursive(3, 32, 4, |inner| {
        prop_oneof![
            // struct: named fields whose values may themselves be `none`
            // (none-in-composite); field names include NUL and empty.
            prop::collection::btree_map(struct_name(), inner.clone(), 0..4).prop_map(|fields| {
                Value::Struct(Struct::new(
                    fields.into_iter().map(|(name, value)| (Text::new(name), value)),
                ))
            }),
            prop::collection::btree_set(inner.clone(), 0..4).prop_map(Value::Set),
            prop::collection::btree_map(inner.clone(), inner.clone(), 0..4).prop_map(Value::Map),
            inner.clone().prop_map(|value| Value::Ref(Ref::scalar(value))),
            prop::collection::vec(inner, 1..4)
                .prop_map(|components| Value::Ref(Ref::composite(components))),
        ]
    })
}

fn struct_name() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("a".to_owned()),
        Just("b".to_owned()),
        Just(String::new()),
        Just("\u{0}".to_owned()),
        small_string(),
    ]
}

fn key_value_strategy() -> impl Strategy<Value = KeyValue> {
    (value_strategy(), prop::collection::vec(value_strategy(), 0..3))
        .prop_map(|(first, rest)| KeyValue::composite(first, rest))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 20_000, ..ProptestConfig::default() })]

    /// memcmp order of two encoded values equals their Annex-B `Ord` — which, for
    /// an order-preserving prefix-free encoding, also entails byte-identical iff
    /// Annex-B-equal.
    #[test]
    fn value_bytea_order_matches_annex_b(a in value_strategy(), b in value_strategy()) {
        let (ea, eb) = (encode(&a), encode(&b));
        prop_assert_eq!(ea.cmp(&eb), a.cmp(&b), "\n a  = {:?}\n b  = {:?}\n ea = {:?}\n eb = {:?}", a, b, ea, eb);
        prop_assert_eq!(ea == eb, a == b, "canonicality\n a = {:?}\n b = {:?}", a, b);
    }

    /// The same property at the production `encode_key_value` entry point, which
    /// concatenates per-component units — so it additionally exercises the
    /// prefix-free framing across keys of differing arity (e.g. `[x]` vs `[x, y]`).
    #[test]
    fn key_value_bytea_order_matches_annex_b(a in key_value_strategy(), b in key_value_strategy()) {
        let (ea, eb) = (key_enc::encode_key_value(&a), key_enc::encode_key_value(&b));
        prop_assert_eq!(ea.cmp(&eb), a.cmp(&b), "\n a = {:?}\n b = {:?}", a, b);
        prop_assert_eq!(ea == eb, a == b);
    }
}
