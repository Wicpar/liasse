//! RED TEAM (Phase 7a): the eval-wire codec on the exotic value edges the shipped
//! `wire_roundtrip.rs` gate does not reach — JSON NUMBERS (its `Json` strategy only
//! draws strings), and wire-form STABILITY (a decoded value must re-encode to the
//! same bytes, so a residual evaluated on the wire can never observe a drifted
//! value).
//!
//! The eval-wire routes `Value::Json` through `serde_json` with `arbitrary_precision`
//! (per the crate manifests), so a large-integer or high-precision JSON number must
//! survive `to_wire → to_string → from_str → from_wire` exactly — else a `json`
//! literal in a hoisted env or a residual would evaluate differently on the wire vs
//! in-process (HIGH). Oracle: `Value`'s own Annex-B equality (the wire's stated
//! contract) plus byte-identity of a re-encode (a stronger, externally-deducible
//! idempotence check).

#![allow(clippy::unwrap_used, clippy::panic)]

use liasse_expr::wire::{env_from_wire, env_to_wire};
use liasse_expr::Cell;
use liasse_value::{EnumValue, Integer, Json, Struct, Text, Value};

fn encode(value: &Value) -> Vec<u8> {
    env_to_wire(&[("x".to_owned(), Cell::scalar(value.clone()))]).unwrap()
}

fn roundtrip(value: &Value) -> Value {
    let decoded = env_from_wire(&encode(value)).unwrap();
    match decoded.into_iter().next() {
        Some((_, Cell::Scalar(value))) => value,
        other => panic!("round-trip did not preserve a scalar cell: {other:?}"),
    }
}

/// Both halves of the wire contract: Annex-B equality AND wire-form stability
/// (`encode(decode(encode v)) == encode v`), which catches a value that decodes
/// equal-by-`cmp` but re-encodes to different bytes (a canonicalization drift a
/// pushed residual would see).
fn assert_stable(value: &Value) {
    let once = roundtrip(value);
    assert_eq!(&once, value, "value did not round-trip by Annex-B equality");
    assert_eq!(encode(&once), encode(value), "wire form is not stable across a round-trip");
}

fn json(text: &str) -> Value {
    Value::Json(Json::from_wire(&serde_json::from_str::<serde_json::Value>(text).unwrap()).unwrap())
}

#[test]
fn json_numbers_survive_arbitrary_precision() {
    // Integers beyond f64's exact range and beyond u64/i64 (10^20 > u64::MAX), and a
    // high-precision fraction — all must survive exactly. If `serde_json` collapsed
    // these through f64 the value would drift.
    for text in [
        "0",
        "-0",
        "123456789012345678901234567890",       // 30-digit integer
        "-99999999999999999999",                 // 20-digit, past u64::MAX
        "3.141592653589793238462643383279",      // 30 significant digits
        "1e-30",
        "9007199254740993",                       // 2^53 + 1, first f64-inexact integer
    ] {
        assert_stable(&json(text));
    }
}

#[test]
fn json_nested_number_and_null_and_bool() {
    // A nested json structure mixing the classes the string-only shipped strategy
    // skips: number, null (distinct from Liasse none), bool, and object/array.
    assert_stable(&json(r#"{"a":1234567890123456789012345,"b":null,"c":[true,-2.5,"x"]}"#));
}

#[test]
fn enum_with_nul_and_high_ordinal_label() {
    // An enum label is arbitrary A.1 text (including U+0000); its ordinal is a u32.
    assert_stable(&Value::Enum(EnumValue::from_parts(0, "a\0b")));
    assert_stable(&Value::Enum(EnumValue::from_parts(u32::MAX, "")));
}

#[test]
fn struct_with_explicit_none_field_survives() {
    // A struct carrying an explicit `none`-valued field: the wire encodes every
    // field (a `none` field is distinct from an absent one at the `Struct` layer),
    // so the round-trip must keep it.
    let value = Value::Struct(Struct::new([
        (Text::new("present"), Value::Int(Integer::from(1))),
        (Text::new("absent"), Value::None),
    ]));
    assert_stable(&value);
}

#[test]
fn deeply_nested_none_as_map_key_and_set_member() {
    // `none` as a map KEY and as a set MEMBER — positions the wire must carry
    // structurally, not by omission.
    let map = Value::Map([(Value::None, Value::Int(Integer::from(9)))].into_iter().collect());
    assert_stable(&map);
    let set = Value::Set([Value::None, Value::Int(Integer::from(1))].into_iter().collect());
    assert_stable(&set);
}
