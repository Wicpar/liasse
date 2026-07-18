//! RED TEAM — Annex B total order and A.9 canonical wire round-trip for the
//! `Value::Composite` positional carrier introduced by the composite-key rework.
//!
//! Every expectation is re-derived from the spec, never from the program's answer:
//! B.4 orders a composite key positionally in `$key` order (distinct from a
//! struct's field-name order), B.2 places `none` after every present value, and the
//! cross-type placement is fixed by the variant rank. A.9 pins the canonical wire as
//! the `$key`-order array of component wire values, with the authoring object form
//! `{ name: … }` accepted and normalized to `$key` order on decode.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use liasse_value::{Decimal, Integer, Ref, Struct, Text, Type, Value};

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}
fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}
fn comp(v: Vec<Value>) -> Value {
    Value::Composite(v)
}

/// Assert ascending: sorting a reversed copy reproduces the input, and each adjacent
/// pair is strictly increasing.
fn assert_ascending(values: Vec<Value>) {
    let mut scrambled: Vec<Value> = values.iter().rev().cloned().collect();
    scrambled.sort();
    assert_eq!(scrambled, values, "sort did not reproduce the expected ascending order");
    for pair in values.windows(2) {
        if let [lo, hi] = pair {
            assert!(lo < hi, "expected {lo:?} < {hi:?}");
        }
    }
}

#[test]
fn composite_orders_positionally_including_arity_and_none() {
    // B.4 positional: first component dominates; a shorter tuple that is a prefix of a
    // longer one sorts first (B.5-style shorter-first); B.2 none sorts after present.
    assert_ascending(vec![
        comp(vec![text("eu")]),                 // arity-1 prefix
        comp(vec![text("eu"), int(2)]),         // shared prefix, extends
        comp(vec![text("eu"), int(10)]),        // second component numeric (2 < 10)
        comp(vec![text("eu"), Value::None]),    // none after any present second value
        comp(vec![text("us"), int(1)]),         // first component dominates (eu < us)
    ]);
}

#[test]
fn composite_scale_variant_components_compare_equal() {
    // B.1 decimal numeric equality inside a positional composite: 1.0 == 1.00.
    let a = comp(vec![text("x"), Value::Decimal(Decimal::parse("1.0").unwrap())]);
    let b = comp(vec![text("x"), Value::Decimal(Decimal::parse("1.00").unwrap())]);
    assert_eq!(a, b, "scale-variant decimals in a composite are one key value");
}

#[test]
fn composite_cross_type_rank_is_between_map_and_none() {
    // The composite variant rank sits after struct/set/map and before none, so a
    // composite of anything outranks a struct and is outranked by none.
    let composite = comp(vec![int(0)]);
    let a_struct = Value::Struct(Struct::new([(Text::new("z"), int(999))]));
    let a_ref = Value::Ref(Ref::scalar(int(999)));
    let a_map = Value::Map(std::collections::BTreeMap::new());
    assert!(a_ref < composite, "ref rank precedes composite");
    assert!(a_struct < composite, "struct rank precedes composite");
    assert!(a_map < composite, "map rank precedes composite");
    assert!(composite < Value::None, "composite precedes none (B.2)");
}

#[test]
fn composite_differs_from_same_field_struct_order() {
    // The very point of the rework: a composite key is NOT ordered like a struct of
    // the same components. $key:[region, code] orders eu:z < us:a (by region), while a
    // name-sorted struct {code, region} orders by `code` first (us:a < eu:z).
    let composite = |region: &str, code: &str| comp(vec![text(region), text(code)]);
    let as_struct = |region: &str, code: &str| {
        Value::Struct(Struct::new([
            (Text::new("region"), text(region)),
            (Text::new("code"), text(code)),
        ]))
    };
    assert!(composite("eu", "z") < composite("us", "a"));
    assert!(as_struct("us", "a") < as_struct("eu", "z"));
}

/// A composite `Type` over `(name, type)` pairs in `$key` order.
fn composite_type(pairs: Vec<(&str, Type)>) -> Type {
    Type::Composite(pairs.into_iter().map(|(n, t)| (n.to_owned(), t)).collect())
}

#[test]
fn composite_wire_round_trips_through_key_order_array() {
    // A.9: to_wire is the $key-order array; Type::Composite decode reproduces it.
    let ty = composite_type(vec![("region", Type::Text), ("code", Type::Int)]);
    let value = comp(vec![text("eu"), int(1)]);
    let wire = value.to_wire();
    assert_eq!(wire, serde_json::json!(["eu", "1"]), "canonical wire is the $key-order array");
    assert_eq!(ty.decode(&wire).unwrap(), value, "array wire round-trips");
}

#[test]
fn composite_wire_round_trips_with_none_and_nested_and_scale() {
    // A positional `none` component (via optional), a nested composite-of-composite,
    // and a scale-bearing decimal all survive the canonical wire round-trip.
    // SPEC-ISSUES item 29: a position cannot be omitted, so the optional slot's
    // `none` is JSON `null` (not the removed `{ "$none": true }` sentinel).
    let ty = composite_type(vec![
        ("a", Type::Optional(Box::new(Type::Text))),
        ("b", composite_type(vec![("x", Type::Int)])),
        ("c", Type::Decimal),
    ]);
    let value = comp(vec![
        Value::None,
        comp(vec![int(7)]),
        Value::Decimal(Decimal::parse("1.50").unwrap()),
    ]);
    let wire = value.to_wire();
    assert_eq!(
        wire,
        serde_json::json!([null, ["7"], "1.50"]),
        "the optional none slot is JSON null in position, not a sentinel object"
    );
    let back = ty.decode(&wire).unwrap();
    assert_eq!(back, value, "composite wire round-trips (none/nested/decimal)");
}

#[test]
fn composite_authoring_object_normalizes_to_key_order() {
    // A.9: the named object selector is accepted and normalized to $key order, so it
    // decodes to the SAME positional tuple the canonical array does.
    let ty = composite_type(vec![("region", Type::Text), ("code", Type::Int)]);
    let object = serde_json::json!({ "code": "1", "region": "eu" }); // authoring order
    let array = serde_json::json!(["eu", "1"]); // $key order
    assert_eq!(
        ty.decode(&object).unwrap(),
        ty.decode(&array).unwrap(),
        "object authoring form normalizes to $key order"
    );
    assert_eq!(ty.decode(&object).unwrap(), comp(vec![text("eu"), int(1)]));
}
