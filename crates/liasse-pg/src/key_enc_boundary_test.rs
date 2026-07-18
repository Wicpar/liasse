//! Red-team explicit-boundary battery for the order-preserving `key_enc` codec.
//!
//! The [companion proptest](crate::key_enc_proptest) draws values from *random*
//! distributions; a defect that lives only on a hand-placed boundary the
//! generators never sample would pass unnoticed. This battery instead
//! *enumerates* the adversarial values explicitly — one representative per
//! cross-type rank boundary, per numeric magnitude-length step, per escaping
//! corner, per decimal/timestamp canonicalization corner, and (crucially) every
//! `period` shape, which the proptest generators omit entirely — and checks the
//! codec's two contract halves over the full Cartesian product:
//!
//! ```text
//! sign(memcmp(encode a, encode b)) == sign(a.cmp(b))       (Annex B order)
//! (encode a == encode b)           == (a == b)             (canonicality)
//! ```
//!
//! `Vec<u8>::cmp` is PostgreSQL's `bytea` order (unsigned memcmp, shorter-first),
//! and `Value`/`KeyValue::cmp` is the derived Annex-B order the reference
//! `MemoryStore` sorts by, so an all-pairs pass over a boundary-dense set is a
//! deterministic witness that the durable key order matches the reference for
//! exactly the corners a random sweep is least likely to hit.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::too_many_lines)]

use core::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use liasse_store::KeyValue;
use liasse_value::bigdecimal::BigDecimal;
use liasse_value::num_bigint::BigInt;
use liasse_value::{
    Ambiguous, BlobDescriptor, Bytes, CalendarPeriodBuilder, Date, Decimal, Duration, EnumValue,
    Integer, Json, MediaType, Missing, Overflow, Period, Precision, Ref, Sha512, Struct, Text,
    Timestamp, Uuid, Value,
};

use crate::key_enc;

fn enc(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    key_enc::encode_value(value, &mut out);
    out
}

fn sign(ordering: Ordering) -> i8 {
    match ordering {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

fn int(value: i128) -> Value {
    Value::Int(Integer::from(BigInt::from(value)))
}

fn dec(text: &str) -> Value {
    Value::Decimal(Decimal::parse(text).expect("valid decimal"))
}

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn bytes(value: &[u8]) -> Value {
    Value::Bytes(Bytes::new(value.to_vec()))
}

fn uuid(fill: [u8; 16]) -> Value {
    Value::Uuid(Uuid::from_bytes(fill))
}

fn date(text: &str) -> Value {
    Value::Date(Date::parse(text).expect("valid date"))
}

fn ts(count: i128, precision: Precision) -> Value {
    Value::Timestamp(Timestamp::new(count, precision))
}

fn dur(nanos: i128) -> Value {
    Value::Duration(Duration::from_nanos(nanos))
}

fn fixed(nanos: i128) -> Value {
    Value::Period(Box::new(Period::Fixed(Duration::from_nanos(nanos))))
}

#[allow(clippy::too_many_arguments)]
fn cal(
    years: i64,
    months: i64,
    weeks: i64,
    days: i64,
    time_nanos: i128,
    zone: Option<&str>,
    overflow: Overflow,
    ambiguous: Ambiguous,
    missing: Missing,
) -> Value {
    let builder = CalendarPeriodBuilder {
        years,
        months,
        weeks,
        days,
        time: Duration::from_nanos(time_nanos),
        zone: zone.map(str::to_owned),
        overflow,
        ambiguous,
        missing,
    };
    Value::Period(Box::new(Period::Calendar(
        builder.build().expect("non-empty calendar period"),
    )))
}

fn enum_of(ordinal: u32, label: &str) -> Value {
    Value::Enum(EnumValue::from_parts(ordinal, label))
}

fn blob(sha_fill: u8, byte_count: u64, media: &str, name: Option<&str>) -> Value {
    let sha = Sha512::parse(&data_encoding::HEXLOWER.encode(&[sha_fill; 64])).expect("valid sha");
    Value::Blob(Box::new(BlobDescriptor::new(
        sha,
        byte_count,
        MediaType::new(media),
        name.map(str::to_owned),
    )))
}

fn strukt(fields: &[(&str, Value)]) -> Value {
    Value::Struct(Struct::new(
        fields
            .iter()
            .map(|(name, value)| (Text::new(*name), value.clone())),
    ))
}

fn set(members: &[Value]) -> Value {
    Value::Set(members.iter().cloned().collect::<BTreeSet<_>>())
}

fn map(entries: &[(Value, Value)]) -> Value {
    Value::Map(entries.iter().cloned().collect::<BTreeMap<_, _>>())
}

fn json_num(text: &str) -> Value {
    Value::Json(Json::Number(text.parse::<BigDecimal>().expect("valid number")))
}

/// Every adversarial single value, labelled. The set is deliberately dense on
/// cross-type rank boundaries, numeric sign/magnitude steps, escaping corners,
/// and the decimal/timestamp canonicalization corners — and carries every
/// `period` shape, which the proptest never generates.
fn battery() -> Vec<(&'static str, Value)> {
    vec![
        // -- bool (rank 0) --
        ("bool:false", Value::Bool(false)),
        ("bool:true", Value::Bool(true)),
        // -- int (rank 1): sign classes + magnitude byte-length steps --
        ("int:i128::MIN", int(i128::MIN)),
        ("int:i64::MIN", int(i128::from(i64::MIN))),
        ("int:-65536", int(-65536)),
        ("int:-65535", int(-65535)),
        ("int:-256", int(-256)),
        ("int:-255", int(-255)),
        ("int:-1", int(-1)),
        ("int:0", int(0)),
        ("int:1", int(1)),
        ("int:255", int(255)),
        ("int:256", int(256)),
        ("int:65535", int(65535)),
        ("int:65536", int(65536)),
        ("int:2^63-1", int((1i128 << 63) - 1)),
        ("int:2^63", int(1i128 << 63)),
        ("int:2^64-1", int((1i128 << 64) - 1)),
        ("int:2^64", int(1i128 << 64)),
        ("int:i64::MAX", int(i128::from(i64::MAX))),
        ("int:i128::MAX", int(i128::MAX)),
        // -- decimal (rank 2): scale variants (canonicality) + magnitude corners --
        ("dec:-100", dec("-100")),
        ("dec:-10", dec("-10")),
        ("dec:-1", dec("-1")),
        ("dec:-0.91", dec("-0.91")),
        ("dec:-0.9", dec("-0.9")),
        ("dec:-0.12", dec("-0.12")),
        ("dec:-0.11", dec("-0.11")),
        ("dec:-0.1", dec("-0.1")),
        ("dec:0", dec("0")),
        ("dec:0.00", dec("0.00")),
        ("dec:0.1", dec("0.1")),
        ("dec:0.12", dec("0.12")),
        ("dec:1", dec("1")),
        ("dec:1.0", dec("1.0")),
        ("dec:1.00", dec("1.00")),
        ("dec:10e-1", dec("10e-1")),
        ("dec:10", dec("10")),
        ("dec:100", dec("100")),
        // near-limit exponents (scale magnitude approaches Decimal::MAX_SCALE_MAGNITUDE = 2^14)
        ("dec:1e-16000", dec("1e-16000")),
        ("dec:1e16000", dec("1e16000")),
        ("dec:-1e16000", dec("-1e16000")),
        // -- text (rank 3): NUL escaping + prefix relationships --
        ("text:empty", text("")),
        ("text:NUL", text("\u{0}")),
        ("text:NUL,NUL", text("\u{0}\u{0}")),
        ("text:a", text("a")),
        ("text:a,NUL", text("a\u{0}")),
        ("text:ab", text("ab")),
        ("text:b", text("b")),
        ("text:astral", text("\u{10348}")),
        // -- bytes (rank 4): 0x00 / 0xFF / 0x00 0xFF / terminator collisions --
        ("bytes:empty", bytes(&[])),
        ("bytes:00", bytes(&[0x00])),
        ("bytes:00,00", bytes(&[0x00, 0x00])),
        ("bytes:00,FF", bytes(&[0x00, 0xFF])),
        ("bytes:00,00,FF", bytes(&[0x00, 0x00, 0xFF])),
        ("bytes:01", bytes(&[0x01])),
        ("bytes:FF", bytes(&[0xFF])),
        ("bytes:FF,00", bytes(&[0xFF, 0x00])),
        // -- uuid (rank 5) --
        ("uuid:zero", uuid([0x00; 16])),
        ("uuid:mid", uuid([0x00, 0xFF, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])),
        ("uuid:max", uuid([0xFF; 16])),
        // -- date (rank 6) --
        ("date:min", date("0001-01-01")),
        ("date:mid", date("2026-07-18")),
        ("date:max", date("9999-12-31")),
        // -- timestamp (rank 7): same-instant cross-precision + i128 extremes --
        ("ts:MIN.ns", ts(i128::MIN, Precision::Nanos)),
        ("ts:-1.ns", ts(-1, Precision::Nanos)),
        ("ts:0.s", ts(0, Precision::Seconds)),
        ("ts:1ms.as-ms", ts(1, Precision::Millis)),
        ("ts:1ms.as-us", ts(1_000, Precision::Micros)),
        ("ts:0.5s.as-ms", ts(500, Precision::Millis)),
        ("ts:0.5s.as-us", ts(500_000, Precision::Micros)),
        ("ts:1s.as-s", ts(1, Precision::Seconds)),
        ("ts:1s.as-ms", ts(1_000, Precision::Millis)),
        ("ts:MAX.s", ts(i128::MAX, Precision::Seconds)),
        // -- duration (rank 8): negatives + extremes --
        ("dur:MIN", dur(i128::MIN)),
        ("dur:-1", dur(-1)),
        ("dur:0", dur(0)),
        ("dur:1", dur(1)),
        ("dur:MAX", dur(i128::MAX)),
        // -- period (rank 9): fixed < calendar; every field/policy axis --
        ("per:fixed:-1", fixed(-1)),
        ("per:fixed:0", fixed(0)),
        ("per:fixed:1", fixed(1)),
        (
            "per:cal:1y",
            cal(1, 0, 0, 0, 0, None, Overflow::Clamp, Ambiguous::Earlier, Missing::Forward),
        ),
        (
            "per:cal:1mo",
            cal(0, 1, 0, 0, 0, None, Overflow::Clamp, Ambiguous::Earlier, Missing::Forward),
        ),
        (
            "per:cal:1d.time1",
            cal(0, 0, 0, 1, 1, None, Overflow::Clamp, Ambiguous::Earlier, Missing::Forward),
        ),
        (
            "per:cal:1d.zoneNone.reject",
            cal(0, 0, 0, 1, 0, None, Overflow::Reject, Ambiguous::Earlier, Missing::Forward),
        ),
        (
            "per:cal:1d.zoneUTC.clamp",
            cal(0, 0, 0, 1, 0, Some("UTC"), Overflow::Clamp, Ambiguous::Earlier, Missing::Forward),
        ),
        (
            "per:cal:1d.zoneUTC.amb-later",
            cal(0, 0, 0, 1, 0, Some("UTC"), Overflow::Clamp, Ambiguous::Later, Missing::Forward),
        ),
        (
            "per:cal:1d.zoneUTC.miss-backward",
            cal(0, 0, 0, 1, 0, Some("UTC"), Overflow::Clamp, Ambiguous::Earlier, Missing::Backward),
        ),
        // -- enum (rank 10): ordinal key + label tiebreak --
        ("enum:0,empty", enum_of(0, "")),
        ("enum:0,a", enum_of(0, "a")),
        ("enum:0,NUL", enum_of(0, "\u{0}")),
        ("enum:1,a", enum_of(1, "a")),
        ("enum:1,b", enum_of(1, "b")),
        // -- ref (rank 11): scalar < composite; composite arity --
        ("ref:scalar:0", Value::Ref(Ref::scalar(int(0)))),
        ("ref:scalar:1", Value::Ref(Ref::scalar(int(1)))),
        ("ref:comp:[0]", Value::Ref(Ref::composite(vec![int(0)]))),
        ("ref:comp:[0,0]", Value::Ref(Ref::composite(vec![int(0), int(0)]))),
        ("ref:comp:[0,1]", Value::Ref(Ref::composite(vec![int(0), int(1)]))),
        // -- json (rank 12): B.3 kind ladder + number canonicality --
        ("json:null", Value::Json(Json::Null)),
        ("json:false", Value::Json(Json::Bool(false))),
        ("json:true", Value::Json(Json::Bool(true))),
        ("json:num:0", json_num("0")),
        ("json:num:1", json_num("1")),
        ("json:num:1.0", json_num("1.0")),
        ("json:num:1.00", json_num("1.00")),
        ("json:str:empty", Value::Json(Json::String(String::new()))),
        ("json:str:a", Value::Json(Json::String("a".to_owned()))),
        ("json:arr:empty", Value::Json(Json::Array(vec![]))),
        ("json:arr:[null]", Value::Json(Json::Array(vec![Json::Null]))),
        ("json:obj:empty", Value::Json(Json::Object(BTreeMap::new()))),
        (
            "json:obj:{a:null}",
            Value::Json(Json::Object(
                [("a".to_owned(), Json::Null)].into_iter().collect(),
            )),
        ),
        // -- blob (rank 13): sha, byte_count, media, optional name (B.4) --
        ("blob:sha00.n1.txt.noname", blob(0x00, 1, "text/plain", None)),
        ("blob:sha00.n1.txt.name-empty", blob(0x00, 1, "text/plain", Some(""))),
        ("blob:sha00.n1.txt.name-a", blob(0x00, 1, "text/plain", Some("a"))),
        ("blob:sha00.n1.pdf.noname", blob(0x00, 1, "text/z", None)),
        ("blob:sha00.n2.txt.noname", blob(0x00, 2, "text/plain", None)),
        ("blob:shaFF.n1.txt.noname", blob(0xFF, 1, "text/plain", None)),
        // -- struct (rank 14): arity, tie-then-differ, none-in-composite --
        ("st:empty", strukt(&[])),
        ("st:{a:none}", strukt(&[("a", Value::None)])),
        ("st:{a:0}", strukt(&[("a", int(0))])),
        ("st:{a:1}", strukt(&[("a", int(1))])),
        ("st:{a:0,b:1}", strukt(&[("a", int(0)), ("b", int(1))])),
        // -- set (rank 15) --
        ("set:empty", set(&[])),
        ("set:{0}", set(&[int(0)])),
        ("set:{1}", set(&[int(1)])),
        ("set:{0,1}", set(&[int(0), int(1)])),
        // -- map (rank 16) --
        ("map:empty", map(&[])),
        ("map:{0:0}", map(&[(int(0), int(0))])),
        ("map:{0:1}", map(&[(int(0), int(1))])),
        ("map:{0:0,1:2}", map(&[(int(0), int(0)), (int(1), int(2))])),
        // -- none (rank 255): the maximum --
        ("none", Value::None),
    ]
}

/// Adversarial multi-component keys: differing arity ([a] vs [a,b]),
/// tie-then-differ, a `none` component, and a NUL-bearing / scale-variant
/// leading component — the framing corners the concatenated key encoder must
/// keep prefix-free.
fn key_battery() -> Vec<(&'static str, KeyValue)> {
    let kv = |first: Value, rest: Vec<Value>| KeyValue::composite(first, rest);
    vec![
        ("[0]", kv(int(0), vec![])),
        ("[0,0]", kv(int(0), vec![int(0)])),
        ("[0,1]", kv(int(0), vec![int(1)])),
        ("[0,1,2]", kv(int(0), vec![int(1), int(2)])),
        ("[1]", kv(int(1), vec![])),
        ("[text a]", kv(text("a"), vec![])),
        ("[text a, text b]", kv(text("a"), vec![text("b")])),
        ("[text '', 0]", kv(text(""), vec![int(0)])),
        ("[text 'a\\0', 0]", kv(text("a\u{0}"), vec![int(0)])),
        ("[none]", kv(Value::None, vec![])),
        ("[none, 0]", kv(Value::None, vec![int(0)])),
        ("[0, none]", kv(int(0), vec![Value::None])),
        ("[dec 1.0]", kv(dec("1.0"), vec![])),
        ("[dec 1.00]", kv(dec("1.00"), vec![])),
        ("[ts 1ms.ms]", kv(ts(1, Precision::Millis), vec![])),
        ("[ts 1ms.us]", kv(ts(1_000, Precision::Micros), vec![])),
    ]
}

#[test]
fn explicit_value_boundaries_match_annex_b() {
    let values = battery();
    let encoded: Vec<Vec<u8>> = values.iter().map(|(_, value)| enc(value)).collect();

    for (i, (label_a, a)) in values.iter().enumerate() {
        for (j, (label_b, b)) in values.iter().enumerate() {
            let value_ord = a.cmp(b);
            let byte_ord = encoded[i].cmp(&encoded[j]);
            assert_eq!(
                sign(byte_ord),
                sign(value_ord),
                "ORDER VIOLATION\n  a  = {label_a}  {a:?}\n  b  = {label_b}  {b:?}\n  Value::cmp = {value_ord:?}\n  memcmp     = {byte_ord:?}\n  ea = {:?}\n  eb = {:?}",
                encoded[i],
                encoded[j],
            );
            assert_eq!(
                encoded[i] == encoded[j],
                a == b,
                "CANONICALITY VIOLATION\n  a  = {label_a}  {a:?}\n  b  = {label_b}  {b:?}\n  a==b = {}\n  ea==eb = {}\n  ea = {:?}\n  eb = {:?}",
                a == b,
                encoded[i] == encoded[j],
                encoded[i],
                encoded[j],
            );
        }
    }
}

/// Concretely witness the canonicality half: values Annex B declares *equal* by
/// externally-deducible arithmetic (a decimal's mathematical value, a timestamp's
/// exact instant, a json number's value) MUST encode to byte-identical keys, or a
/// durable lookup by a scale/precision-variant address would miss its row. This
/// is not tautological — the equality is deduced from B.1/B.3 mathematics, and
/// byte-identity is the codec's independent promise the assertion pins.
#[test]
fn canonicalization_collapses_annex_b_equal_variants() {
    let equal_groups: &[&[Value]] = &[
        // decimal: 1 == 1.0 == 1.00 == 10 x 10^-1 (B.1 mathematical value)
        &[dec("1"), dec("1.0"), dec("1.00"), dec("10e-1")],
        // decimal: signed zero across scales
        &[dec("0"), dec("0.0"), dec("0.00"), dec("-0")],
        // decimal: trailing-zero significand collapse
        &[dec("100"), dec("1e2"), dec("1.00e2")],
        // timestamp: one instant spelled at four precisions (B.1 exact instant)
        &[
            ts(1, Precision::Seconds),
            ts(1_000, Precision::Millis),
            ts(1_000_000, Precision::Micros),
            ts(1_000_000_000, Precision::Nanos),
        ],
        // timestamp: sub-second instant the proptest never collides
        &[ts(1, Precision::Millis), ts(1_000, Precision::Micros)],
        // json number: value-equal across scale (B.3 mathematical)
        &[json_num("1"), json_num("1.0"), json_num("1.00")],
    ];

    for group in equal_groups {
        let first = &group[0];
        let first_bytes = enc(first);
        for variant in group.iter() {
            assert_eq!(
                variant, first,
                "battery bug: {variant:?} and {first:?} are not Annex-B-equal",
            );
            assert_eq!(
                enc(variant),
                first_bytes,
                "CANONICALITY VIOLATION: Annex-B-equal {variant:?} and {first:?} encode differently\n  e(variant) = {:?}\n  e(first)   = {:?}",
                enc(variant),
                first_bytes,
            );
        }
    }
}

#[test]
fn explicit_keyvalue_boundaries_match_annex_b() {
    let keys = key_battery();
    let encoded: Vec<Vec<u8>> = keys
        .iter()
        .map(|(_, key)| key_enc::encode_key_value(key))
        .collect();

    for (i, (label_a, a)) in keys.iter().enumerate() {
        for (j, (label_b, b)) in keys.iter().enumerate() {
            let key_ord = a.cmp(b);
            let byte_ord = encoded[i].cmp(&encoded[j]);
            assert_eq!(
                sign(byte_ord),
                sign(key_ord),
                "KEY ORDER VIOLATION\n  a = {label_a}\n  b = {label_b}\n  KeyValue::cmp = {key_ord:?}\n  memcmp        = {byte_ord:?}\n  ea = {:?}\n  eb = {:?}",
                encoded[i],
                encoded[j],
            );
            assert_eq!(
                encoded[i] == encoded[j],
                a == b,
                "KEY CANONICALITY VIOLATION\n  a = {label_a}\n  b = {label_b}",
            );
        }
    }
}
