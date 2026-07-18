//! Red-team probe of the positional composite key/ref representation landed in
//! f3a21bc (`Value::Composite`, `RefKey::Composite`, `Type::Composite`,
//! `RefTarget::Composite`). Every expectation is re-derived from SPEC.md, never
//! echoed from the implementation:
//!
//! - Annex B.4 "composite key — lexicographic components in `$key` order",
//!   which is DISTINCT from a struct's field-name order.
//! - Annex B.1 per-component type order (int numeric, timestamp signed, text
//!   Unicode scalar) and "numerically equal canonical values compare equal".
//! - Annex A.9 "A composite key uses an array of component wire values in `$key`
//!   order; named object selectors are authoring syntax for the same typed
//!   tuple." (object member order carries no meaning; it normalizes to `$key`
//!   order.)
//! - §6.3 "a set contributes keys in the target collection's canonical order".
//!
//! The angles here go beyond the two-component text+int cases already in
//! `order.rs`/`wire.rs`: 3+ components with a leading tie, mixed component types,
//! a struct component, a decimal-scale variant inside a component, the
//! object-form-reordered wire normalization, encode->decode identity, and the
//! arity/missing/extra error paths.

use serde_json::json;

use liasse_value::{
    Decimal, Integer, Precision, Ref, RefTarget, Struct, Text, Timestamp, Type, Value, ValueError,
};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

fn ts(seconds: i128) -> Value {
    Value::Timestamp(Timestamp::new(seconds, Precision::Seconds))
}

/// Assert `values` is strictly ascending under `Value`'s Annex-B order and that
/// sorting a reversed copy reproduces it (defeats an already-sorted input).
fn assert_ascending(values: Vec<Value>) {
    let mut scrambled: Vec<Value> = values.iter().rev().cloned().collect();
    scrambled.sort();
    assert_eq!(scrambled, values, "sorting a reversed copy must reproduce ascending order");
    for pair in values.windows(2) {
        if let [lo, hi] = pair {
            assert!(lo < hi, "expected {lo:?} < {hi:?}");
        }
    }
}

/// B.4 composite key over THREE components with mixed types, where the first
/// (and, for one pair, the first two) components tie so a later component
/// decides. `$key: [region:text, seq:int, at:timestamp]`.
///
/// Deducible from B.4 (lexicographic in `$key` order) + B.1 (int numeric so
/// 2 < 10, NOT text "10" < "2"; timestamp signed; text Unicode scalar).
#[test]
fn composite_three_components_first_ties_later_decides() {
    let key = |region: &str, seq: i64, at: i128| {
        Value::Composite(vec![text(region), int(seq), ts(at)])
    };
    assert_ascending(vec![
        key("eu", 2, 100),  // baseline
        key("eu", 2, 200),  // region+seq tie -> timestamp 100 < 200
        key("eu", 10, 50),  // region ties -> seq 2 < 10 (numeric, not text)
        key("us", 1, 0),    // region eu < us dominates every later component
    ]);
}

/// The very same three components carried as a name-sorted struct order by
/// field-name (`at` < `region` < `seq`), NOT by `$key` position — proving the
/// two B.4 rows are distinct rules and that `Value::Composite` realizes the
/// composite-key rule, not the struct rule.
#[test]
fn composite_order_is_not_struct_field_name_order() {
    // Field names {code, region} sort as [code, region], but $key order is
    // [region, code] — so the two rules disagree on the same components.
    let comp = |region: &str, code: &str| Value::Composite(vec![text(region), text(code)]);
    let strc = |region: &str, code: &str| {
        Value::Struct(Struct::new([
            (Text::new("region"), text(region)),
            (Text::new("code"), text(code)),
        ]))
    };
    // Composite (by region): eu:z < us:a. Struct (by field name code): us:a < eu:z.
    assert!(comp("eu", "z") < comp("us", "a"));
    assert!(strc("us", "a") < strc("eu", "z"));
}

/// B.1 "numerically equal canonical values compare equal" holds inside a
/// component: a decimal-scale variant (1.0 vs 1.00) is one key value. Under
/// SPEC-ISSUES item 1 the canonical wire is minimal scale — a total function of
/// the value — so the two variants also produce the *same* wire text.
#[test]
fn composite_decimal_scale_variant_component_is_one_key() -> Result<(), ValueError> {
    let a = Value::Composite(vec![Value::Decimal(Decimal::parse("1.0")?), int(5)]);
    let b = Value::Composite(vec![Value::Decimal(Decimal::parse("1.00")?), int(5)]);
    assert_eq!(a, b, "scale-variant decimals are one composite key (B.1)");
    // One canonical spelling per value: both minimal-scale to "1" (A.1).
    assert_eq!(a.to_wire(), json!(["1", "5"]));
    assert_eq!(b.to_wire(), json!(["1", "5"]));
    Ok(())
}

/// A struct component orders by field-name order WITHIN the component, while the
/// composite orders by `$key` position ACROSS components — the two B.4 rows
/// compose. `$key: [loc:struct{city,zip}, tier:int]`.
#[test]
fn composite_with_struct_component_composes_both_b4_rules() {
    let key = |city: &str, zip: &str, tier: i64| {
        Value::Composite(vec![
            Value::Struct(Struct::new([
                (Text::new("city"), text(city)),
                (Text::new("zip"), text(zip)),
            ])),
            int(tier),
        ])
    };
    assert_ascending(vec![
        key("paris", "75001", 9), // struct component (city then zip) leads
        key("paris", "75002", 1), // city ties -> zip 75001 < 75002 dominates tier
        key("paris", "75002", 5), // struct ties -> tier 1 < 5
        key("zurich", "8001", 1), // city paris < zurich dominates the rest
    ]);
}

/// A.9 wire form of a bare composite key value is the `$key`-order array of
/// component wire values. `int` components render as A.1 JSON strings.
#[test]
fn composite_wire_is_key_order_array() {
    let value = Value::Composite(vec![text("eu"), int(5)]);
    assert_eq!(value.to_wire(), json!(["eu", "5"]));
}

/// A.9 normalization: decoding the canonical `$key`-order array, the authoring
/// object `{region, code}`, and the SAME object with members in a DIFFERENT
/// order all yield the identical `$key`-order tuple. Object member order carries
/// no meaning. `$key: [region:text, code:int]`.
#[test]
fn composite_decode_array_and_reordered_object_normalize_identically() -> Result<(), ValueError> {
    let ty = Type::Composite(vec![
        ("region".to_owned(), Type::Text),
        ("code".to_owned(), Type::Int),
    ]);
    let expected = Value::Composite(vec![text("eu"), int(5)]);

    let from_array = ty.decode(&json!(["eu", "5"]))?;
    let from_object = ty.decode(&json!({ "region": "eu", "code": "5" }))?;
    // Object members deliberately in the OPPOSITE order to `$key`.
    let from_reordered = ty.decode(&json!({ "code": "5", "region": "eu" }))?;

    assert_eq!(from_array, expected);
    assert_eq!(from_object, expected);
    assert_eq!(from_reordered, expected, "object member order must not affect the tuple");
    Ok(())
}

/// encode(to_wire) -> decode is the identity on a composite key value.
#[test]
fn composite_wire_round_trip_is_identity() -> Result<(), ValueError> {
    let ty = Type::Composite(vec![
        ("region".to_owned(), Type::Text),
        ("code".to_owned(), Type::Int),
    ]);
    let value = Value::Composite(vec![text("eu"), int(5)]);
    let round_tripped = ty.decode(&value.to_wire())?;
    assert_eq!(round_tripped, value);
    Ok(())
}

/// A.9 arity/shape enforcement: a wrong-arity array, an object missing a
/// component, and an object with an extra member are all rejected.
#[test]
fn composite_decode_rejects_wrong_arity_and_bad_members() {
    let ty = Type::Composite(vec![
        ("region".to_owned(), Type::Text),
        ("code".to_owned(), Type::Int),
    ]);
    // Too few array elements.
    assert!(matches!(
        ty.decode(&json!(["eu"])),
        Err(ValueError::CompositeArity { expected: 2, found: 1 })
    ));
    // Too many array elements.
    assert!(matches!(
        ty.decode(&json!(["eu", "5", "x"])),
        Err(ValueError::CompositeArity { expected: 2, found: 3 })
    ));
    // Object missing a declared component.
    assert!(matches!(
        ty.decode(&json!({ "region": "eu" })),
        Err(ValueError::MissingMember(name)) if name == "code"
    ));
    // Object carrying an undeclared member.
    assert!(matches!(
        ty.decode(&json!({ "region": "eu", "code": "5", "extra": "x" })),
        Err(ValueError::UnexpectedMember(name)) if name == "extra"
    ));
}

/// B.1 `ref<T>`: "ascending is target-key order"; A.9: a composite ref's key is
/// the component array in `$key` order. So refs sort by their target's composite
/// key components positionally, and the ref's wire value is that same array.
/// `$key: [region:text, code:int]`.
#[test]
fn composite_ref_orders_by_target_key_and_wires_as_array() -> Result<(), ValueError> {
    let ref_key = |region: &str, code: i64| Value::Ref(Ref::composite(vec![text(region), int(code)]));
    // region decides first; within a region, int order on code (2 < 10).
    assert_ascending(vec![
        ref_key("eu", 2),
        ref_key("eu", 10),
        ref_key("us", 1),
    ]);

    // A.9 wire form is the $key-order array (not "eu:2", not {region, code}).
    assert_eq!(ref_key("eu", 2).to_wire(), json!(["eu", "2"]));

    // A composite ref decodes from BOTH the array and the authoring object.
    let target = RefTarget::Composite(vec![
        ("region".to_owned(), Type::Text),
        ("code".to_owned(), Type::Int),
    ]);
    let from_array = Type::Ref(target.clone()).decode(&json!(["eu", "2"]))?;
    let from_object = Type::Ref(target).decode(&json!({ "code": "2", "region": "eu" }))?;
    assert_eq!(from_array, ref_key("eu", 2));
    assert_eq!(from_object, ref_key("eu", 2), "object authoring form normalizes to the tuple");
    Ok(())
}

/// §6.3 / B.4: a set of composite keys enumerates its members in the target's
/// canonical order — B.4 composite-key order (`$key` positional), regardless of
/// insertion order. Members are seeded in reverse and with names whose field
/// order disagrees with `$key` order.
#[test]
fn set_of_composites_orders_in_key_order() {
    use std::collections::BTreeSet;
    let comp = |region: &str, code: &str| Value::Composite(vec![text(region), text(code)]);
    // Insert in reverse of $key order: us:a then eu:z.
    let members: BTreeSet<Value> = [comp("us", "a"), comp("eu", "z")].into_iter().collect();
    let set = Value::Set(members);
    // Canonical order is eu:z before us:a (region decides), each as its array.
    assert_eq!(set.to_wire(), json!([["eu", "z"], ["us", "a"]]));
}

/// Total-order sanity for the new `Value::Composite` variant's cross-type rank:
/// it must sit at a fixed, deterministic point relative to `Struct`, `Ref`,
/// `Set`, and `Map`, and `None` must remain the maximum (B.2). Cross-type order
/// is not spec-pinned (a sort column is single-typed) but MUST be a stable total
/// order — and must agree with the `key_enc` byte rank the pg proptest gates.
#[test]
fn composite_cross_type_rank_is_stable_and_none_is_max() {
    let composite = Value::Composite(vec![text("a"), int(1)]);
    let a_struct = Value::Struct(Struct::new([(Text::new("a"), int(1))]));
    let a_ref = Value::Ref(Ref::composite(vec![text("a"), int(1)]));
    let a_set = Value::Set([text("a")].into_iter().collect());
    let a_map = Value::Map([(text("a"), int(1))].into_iter().collect());

    // key_enc rank bytes: ref=0x0B, struct=0x0E, set=0x0F, map=0x10, composite=0x11.
    // Value::rank must agree with that ordering.
    assert!(a_ref < a_struct);
    assert!(a_struct < a_set);
    assert!(a_set < a_map);
    assert!(a_map < composite);
    // B.2: none is the maximum present-or-absent value.
    assert!(composite < Value::None);
}
