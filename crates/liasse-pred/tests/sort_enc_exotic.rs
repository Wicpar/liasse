//! RED TEAM (Phase 7a): `sort_enc` ≡ Annex-B tuple comparison for the EXOTIC and
//! MIXED scalar classes the shipped `sort_enc.rs` gate never reaches.
//!
//! `crates/liasse-pg/src/sort_enc.rs` claims (module doc, lines 6-12) that
//! `key_enc::encode_value` is "prefix-free-and-canonical … across every `Value`
//! class", so whole-unit byte inversion for a descending key reproduces the reversed
//! `Value::cmp` INCLUDING `none` placement. The shipped proptest only draws from
//! `{bool, int, decimal, text, timestamp, none}`, and `key_enc`'s OWN proptest runs
//! over `KeyValue` (key-eligible values). But a `$sort` key can evaluate to a
//! NON-key-eligible scalar — a `duration`, a `period`, a `json`, a `blob`, a
//! `ref`, an `enum`, a `uuid`, a `bytes`, a `date`, or a structured `set`/`map`/
//! `struct`/`composite` `Cell::Scalar` — which `encode_sort_tuple` still feeds
//! through `key_enc`. Those classes are the untested surface this file attacks.
//!
//! Oracle: `Value`'s own Annex-B `Ord`, reversed per descending key, `none`
//! deferring to `Ordering::Equal` on a missing component — identical to the
//! reference in `sort_enc.rs` and to `SortOrder::compare`/`compare_sorted`, and
//! externally deducible from Annex B.2/§7.3 (AGENTS.md). A `sign` mismatch between
//! `memcmp(sort_enc(a), sort_enc(b))` and `tuple_cmp(a, b)` = a pushed `ORDER BY`
//! that disagrees with the in-Rust oracle = HIGH.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use core::cmp::Ordering;

use liasse_pg::encode_sort_tuple;
use liasse_store::SortDirection;
use liasse_value::bigdecimal::BigDecimal;
use liasse_value::num_bigint::BigInt;
use liasse_value::{
    Bytes, CalendarPeriodBuilder, Date, Decimal, Duration, EnumValue, Integer, Json, MediaType,
    Period, Precision, Ref, Sha512, Struct, Text, Timestamp, Uuid, Value,
};
use proptest::prelude::*;

/// The reference sort-tuple comparison: successive keys under Annex-B `Ord`, each
/// descending key reversed, a missing component tying (no occurrence tiebreak).
/// `Value::None` is the Annex-B.2 rank maximum, so it sorts last ascending and
/// first descending purely through `Value::cmp` — the encoder must reproduce that.
fn tuple_cmp(a: &[Value], b: &[Value], dirs: &[SortDirection]) -> Ordering {
    for (index, dir) in dirs.iter().enumerate() {
        let ordering = match (a.get(index), b.get(index)) {
            (Some(x), Some(y)) => x.cmp(y),
            _ => Ordering::Equal,
        };
        let ordering = match dir {
            SortDirection::Ascending => ordering,
            SortDirection::Descending => ordering.reverse(),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

fn assert_consistent(a: &[Value], b: &[Value], dirs: &[SortDirection]) {
    let ea = encode_sort_tuple(a, dirs);
    let eb = encode_sort_tuple(b, dirs);
    assert_eq!(
        ea.cmp(&eb),
        tuple_cmp(a, b, dirs),
        "\nsort_enc memcmp disagrees with Annex-B tuple_cmp\n a    = {a:?}\n b    = {b:?}\n dirs = {dirs:?}\n ea   = {ea:?}\n eb   = {eb:?}",
    );
}

// --- strategies over the exotic scalar classes ----------------------------------

fn pow10(exponent: u32) -> BigInt {
    (0..exponent).fold(BigInt::from(1i64), |acc, _| acc * BigInt::from(10i64))
}

fn edge_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![Just('\u{0}'), Just('\u{1}'), Just('a'), Just('\u{ff}'), Just('é'), any::<char>()],
        0..4,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn calendar_period(years: i64, days: i64, zone: Option<&str>) -> Value {
    let mut builder = CalendarPeriodBuilder {
        years,
        days,
        time: Duration::from_nanos(0),
        zone: zone.map(str::to_owned),
        ..CalendarPeriodBuilder::default()
    };
    // Guarantee A.4's non-zero-magnitude invariant when both magnitudes are zero.
    if years == 0 && days == 0 {
        builder.days = 1;
    }
    Value::Period(Box::new(Period::Calendar(builder.build().unwrap())))
}

/// Every scalar class that can surface as a `$sort` value, heavy on the edges each
/// class's `Ord` or `key_enc` layout is most likely to disagree on.
fn exotic_scalar() -> BoxedStrategy<Value> {
    prop_oneof![
        any::<bool>().prop_map(Value::Bool),
        (-5i64..5).prop_map(|n| Value::Int(Integer::from(n))),
        // Mixed-scale decimals incl. negatives: `1`, `1.0`, `1.00` are one value.
        (-50i64..50, -3i64..4, 0u32..3).prop_map(|(m, s, pad)| Value::Decimal(
            Decimal::from_big_decimal(BigDecimal::new(BigInt::from(m) * pow10(pad), s + i64::from(pad)))
        )),
        edge_text().prop_map(|t| Value::Text(Text::new(t))),
        prop::collection::vec(prop_oneof![Just(0u8), Just(1u8), Just(255u8), any::<u8>()], 0..4)
            .prop_map(|b| Value::Bytes(Bytes::new(b))),
        prop::array::uniform16(prop_oneof![Just(0u8), Just(255u8), any::<u8>()])
            .prop_map(|b| Value::Uuid(Uuid::from_bytes(b))),
        (1i32..9999, 1u32..=12, 1u32..=28)
            .prop_map(|(y, m, d)| Value::Date(Date::parse(&format!("{y:04}-{m:02}-{d:02}")).unwrap())),
        // Sub-second cross-precision instants: the shipped gate only used whole
        // seconds, so a fractional-second precision collision is uncovered.
        (-100i64..100, prop_oneof![Just(Precision::Seconds), Just(Precision::Millis), Just(Precision::Micros), Just(Precision::Nanos)])
            .prop_map(|(t, p)| Value::Timestamp(Timestamp::new(i128::from(t), p))),
        (-5i64..5).prop_map(|n| Value::Duration(Duration::from_nanos(i128::from(n)))),
        (-3i64..3).prop_map(|n| Value::Period(Box::new(Period::Fixed(Duration::from_nanos(i128::from(n)))))),
        // Calendar period: exercises the `none`-last optional `zone` tag under
        // whole-unit descending inversion.
        (-2i64..2, -2i64..2, prop_oneof![Just(None), Just(Some("Z")), Just(Some("Europe/Paris"))])
            .prop_map(|(y, d, z)| calendar_period(y, d, z)),
        (0u32..3, edge_text()).prop_map(|(o, l)| Value::Enum(EnumValue::from_parts(o, l))),
        (-5i64..5).prop_map(|n| Value::Ref(Ref::scalar(Value::Int(Integer::from(n))))),
        prop::collection::vec((-3i64..3).prop_map(|n| Value::Int(Integer::from(n))), 1..3)
            .prop_map(|c| Value::Ref(Ref::composite(c))),
        (-5i64..5).prop_map(|n| Value::Json(Json::from_wire(&serde_json::json!(n)).unwrap())),
        edge_text().prop_map(|s| Value::Json(Json::from_wire(&serde_json::Value::String(s)).unwrap())),
        blob_value(),
        Just(Value::None),
    ]
    .boxed()
}

fn blob_value() -> impl Strategy<Value = Value> {
    (prop_oneof![Just(0u8), Just(7u8), Just(255u8)], 0u64..3, prop_oneof![Just(None), Just(Some("a.txt"))]).prop_map(
        |(fill, bytes, name)| {
            let sha = Sha512::parse(&format!("{fill:02x}").repeat(64)).unwrap();
            Value::Blob(Box::new(liasse_value::BlobDescriptor::new(
                sha,
                bytes,
                MediaType::new("text/plain".to_owned()),
                name.map(str::to_owned),
            )))
        },
    )
}

/// A structured `Cell::Scalar` value (set/map/struct/composite) that `as_scalar`
/// still admits as a sort key, exercising the framed-sequence encoding under
/// inversion.
fn structured_scalar() -> BoxedStrategy<Value> {
    let leaf = (-3i64..3).prop_map(|n| Value::Int(Integer::from(n)));
    prop_oneof![
        prop::collection::btree_set(leaf.clone(), 0..3).prop_map(Value::Set),
        prop::collection::btree_map(leaf.clone(), leaf.clone(), 0..3).prop_map(Value::Map),
        prop::collection::vec(leaf.clone(), 1..3).prop_map(Value::Composite),
        prop::collection::btree_map(edge_text(), leaf, 0..3)
            .prop_map(|f| Value::Struct(Struct::new(f.into_iter().map(|(n, v)| (Text::new(n), v))))),
    ]
    .boxed()
}

fn direction() -> impl Strategy<Value = SortDirection> {
    prop_oneof![Just(SortDirection::Ascending), Just(SortDirection::Descending)]
}

fn same_arity_case(
    value: impl Strategy<Value = Value> + Clone,
) -> impl Strategy<Value = (Vec<Value>, Vec<Value>, Vec<SortDirection>)> {
    (1usize..4).prop_flat_map(move |arity| {
        (
            prop::collection::vec(value.clone(), arity),
            prop::collection::vec(value.clone(), arity),
            prop::collection::vec(direction(), arity),
        )
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12000, ..ProptestConfig::default() })]

    /// The full exotic scalar universe: every class, mixed within a tuple, both
    /// directions, `none` interleaved.
    #[test]
    fn exotic_scalars_memcmp_equals_tuple_cmp((a, b, dirs) in same_arity_case(exotic_scalar())) {
        let ea = encode_sort_tuple(&a, &dirs);
        let eb = encode_sort_tuple(&b, &dirs);
        prop_assert_eq!(ea.cmp(&eb), tuple_cmp(&a, &b, &dirs),
            "\n a = {:?}\n b = {:?}\n dirs = {:?}", a, b, dirs);
    }

    /// The structured `Cell::Scalar` classes (framed sequences) under inversion.
    #[test]
    fn structured_scalars_memcmp_equals_tuple_cmp((a, b, dirs) in same_arity_case(structured_scalar())) {
        let ea = encode_sort_tuple(&a, &dirs);
        let eb = encode_sort_tuple(&b, &dirs);
        prop_assert_eq!(ea.cmp(&eb), tuple_cmp(&a, &b, &dirs),
            "\n a = {:?}\n b = {:?}\n dirs = {:?}", a, b, dirs);
    }
}

// --- explicit edge unit cases the task enumerates -------------------------------

fn dec(text: &str) -> Value {
    Value::Decimal(Decimal::parse(text).unwrap())
}
fn ts(count: i128, p: Precision) -> Value {
    Value::Timestamp(Timestamp::new(count, p))
}
fn txt(s: &str) -> Value {
    Value::Text(Text::new(s))
}

#[test]
fn descending_decimal_scale_variants_collide() {
    // `1.0` and `1.00` are one value; descending must keep them tied, not order
    // them by a phantom scale.
    let d = [SortDirection::Descending];
    for (x, y) in [("1.0", "1.00"), ("1", "1.000"), ("-2.50", "-2.5"), ("0", "0.00")] {
        assert_consistent(&[dec(x)], &[dec(y)], &d);
        assert_eq!(
            encode_sort_tuple(&[dec(x)], &d),
            encode_sort_tuple(&[dec(y)], &d),
            "scale variants {x} / {y} must encode byte-identically (descending)",
        );
    }
}

#[test]
fn none_placement_multi_key_both_directions() {
    // `none` in a leading key, both directions, with a present second key.
    let present = [Value::None, Value::Int(Integer::from(1))];
    let also = [Value::None, Value::Int(Integer::from(2))];
    let one = [Value::Int(Integer::from(1)), Value::None];
    for dirs in [
        vec![SortDirection::Ascending, SortDirection::Ascending],
        vec![SortDirection::Descending, SortDirection::Ascending],
        vec![SortDirection::Ascending, SortDirection::Descending],
        vec![SortDirection::Descending, SortDirection::Descending],
    ] {
        assert_consistent(&present, &also, &dirs);
        assert_consistent(&present, &one, &dirs);
        assert_consistent(&one, &also, &dirs);
    }
}

#[test]
fn none_is_max_ascending_min_descending() {
    // none sorts AFTER any present value ascending, BEFORE it descending — the
    // reversal of Value::None being the Annex-B.2 rank maximum.
    let present = [Value::Int(Integer::from(-9))];
    let none = [Value::None];
    let asc = [SortDirection::Ascending];
    let desc = [SortDirection::Descending];
    assert_eq!(encode_sort_tuple(&present, &asc).cmp(&encode_sort_tuple(&none, &asc)), Ordering::Less);
    assert_eq!(encode_sort_tuple(&present, &desc).cmp(&encode_sort_tuple(&none, &desc)), Ordering::Greater);
    assert_consistent(&present, &none, &asc);
    assert_consistent(&present, &none, &desc);
}

#[test]
fn descending_then_ascending_composite() {
    // A descending key followed by an ascending key: the two directions must not
    // bleed across the unit boundary.
    let dirs = [SortDirection::Descending, SortDirection::Ascending];
    let cases = [
        ([Value::Int(Integer::from(1)), txt("a")], [Value::Int(Integer::from(1)), txt("b")]),
        ([Value::Int(Integer::from(2)), txt("a")], [Value::Int(Integer::from(1)), txt("z")]),
        ([Value::Int(Integer::from(1)), txt("a")], [Value::Int(Integer::from(2)), txt("a")]),
    ];
    for (a, b) in cases {
        assert_consistent(&a, &b, &dirs);
    }
}

#[test]
fn timestamp_precision_variants_collide() {
    // Whole and sub-second cross-precision instants: `(1000, ms)` == `(1, s)`,
    // `(1_500_000, us)` == `(1500, ms)`, both directions.
    for dir in [SortDirection::Ascending, SortDirection::Descending] {
        let d = [dir];
        assert_consistent(&[ts(1000, Precision::Millis)], &[ts(1, Precision::Seconds)], &d);
        assert_consistent(&[ts(1_500_000, Precision::Micros)], &[ts(1500, Precision::Millis)], &d);
        assert_eq!(
            encode_sort_tuple(&[ts(1000, Precision::Millis)], &d),
            encode_sort_tuple(&[ts(1, Precision::Seconds)], &d),
        );
    }
}

#[test]
fn nul_and_edge_byte_text_keys() {
    // NUL-bearing and high-byte text must stay order-preserving under the escape
    // encoding, both directions.
    for dir in [SortDirection::Ascending, SortDirection::Descending] {
        let d = [dir];
        for (x, y) in [("\0", "\0\0"), ("a", "a\0"), ("\u{ff}", "\u{ff}\u{ff}"), ("", "\0")] {
            assert_consistent(&[txt(x)], &[txt(y)], &d);
        }
    }
}

#[test]
fn bool_key_both_directions() {
    for dir in [SortDirection::Ascending, SortDirection::Descending] {
        let d = [dir];
        assert_consistent(&[Value::Bool(false)], &[Value::Bool(true)], &d);
        assert_consistent(&[Value::Bool(true)], &[Value::None], &d);
    }
}

#[test]
fn enum_ordinal_ordering() {
    // Enum orders by ordinal then label; distinct ordinals must order by ordinal.
    for dir in [SortDirection::Ascending, SortDirection::Descending] {
        let d = [dir];
        let lo = [Value::Enum(EnumValue::from_parts(0, "z"))];
        let hi = [Value::Enum(EnumValue::from_parts(1, "a"))];
        assert_consistent(&lo, &hi, &d);
    }
}
