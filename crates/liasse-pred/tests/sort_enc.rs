//! `sort_enc`'s memcmp order equals the shared Annex-B sort-tuple comparison (§7.4).
//!
//! The pushdown emits `sort_enc(tuple)` as the `ORDER BY` key, while the in-Rust
//! oracle sorts by successive keys under `Value`'s Annex-B `Ord`, reversing each
//! descending key (§7.3). The two MUST agree, or a pushed sort and an oracle sort
//! deliver rows in different orders. The gate: for random tuple pairs and direction
//! vectors,
//!
//! ```text
//! sign(memcmp(sort_enc(a, dirs), sort_enc(b, dirs))) == sign(tuple_cmp(a, b, dirs))
//! ```
//!
//! `Vec<u8>`'s `Ord` IS PostgreSQL's `bytea` memcmp order. The reference
//! `tuple_cmp` is hand-derived from Annex B.2/§7.3 (present ascending then `none`,
//! reversed for a descending key) — externally deducible, per AGENTS.md. The
//! occurrence tiebreak is deliberately unencoded, so tied tuples compare `Equal`.

#![allow(clippy::unwrap_used, clippy::panic)]

use core::cmp::Ordering;

use liasse_pg::encode_sort_tuple;
use liasse_store::SortDirection;
use liasse_value::bigdecimal::BigDecimal;
use liasse_value::num_bigint::BigInt;
use liasse_value::{Decimal, Integer, Text, Timestamp, Value};
use proptest::prelude::*;

/// The reference sort-tuple comparison: successive keys under Annex-B `Ord`, each
/// descending key reversed, tied when all keys agree (no occurrence tiebreak).
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

fn pow10(exponent: u32) -> BigInt {
    (0..exponent).fold(BigInt::from(1i64), |acc, _| acc * BigInt::from(10i64))
}

/// A mix of the ordered scalar classes plus `none`, heavy on the adversarial edges:
/// mixed-scale decimals (`1` = `1.0`), NUL-bearing text, and every timestamp
/// precision (whole-second instants that must collide across precisions).
fn sort_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Int(Integer::from(n))),
        (-10_000i64..10_000, -4i64..6, 0u32..3).prop_map(|(m, s, pad)| {
            Value::Decimal(Decimal::from_big_decimal(BigDecimal::new(
                BigInt::from(m) * pow10(pad),
                s + i64::from(pad),
            )))
        }),
        prop::collection::vec(prop_oneof![Just('\u{0}'), Just('a'), any::<char>()], 0..4)
            .prop_map(|c| Value::Text(Text::new(c.into_iter().collect::<String>()))),
        (-1000i64..1000, prop_oneof![Just(1i128), Just(1000), Just(1_000_000)])
            .prop_map(|(whole, ticks)| Value::Timestamp(Timestamp::new(
                i128::from(whole) * ticks,
                match ticks {
                    1 => liasse_value::Precision::Seconds,
                    1000 => liasse_value::Precision::Millis,
                    _ => liasse_value::Precision::Micros,
                },
            ))),
        Just(Value::None),
    ]
}

fn direction() -> impl Strategy<Value = SortDirection> {
    prop_oneof![Just(SortDirection::Ascending), Just(SortDirection::Descending)]
}

/// Two same-arity tuples and one direction vector of that arity — exactly how a
/// single view's `$sort` (a fixed key count) compares any two of its rows.
fn same_arity_case() -> impl Strategy<Value = (Vec<Value>, Vec<Value>, Vec<SortDirection>)> {
    (1usize..4).prop_flat_map(|arity| {
        (
            prop::collection::vec(sort_value(), arity),
            prop::collection::vec(sort_value(), arity),
            prop::collection::vec(direction(), arity),
        )
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 8000, ..ProptestConfig::default() })]

    #[test]
    fn memcmp_equals_tuple_cmp((a, b, dirs) in same_arity_case()) {
        let ea = encode_sort_tuple(&a, &dirs);
        let eb = encode_sort_tuple(&b, &dirs);
        prop_assert_eq!(ea.cmp(&eb), tuple_cmp(&a, &b, &dirs), "\n a = {:?}\n b = {:?}\n dirs = {:?}", a, b, dirs);
    }
}
