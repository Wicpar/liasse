//! RED TEAM — SPEC-ISSUES #29 / Annex A.1 at the DECODE boundary: a `none` must
//! never be stored as a `map` value or a `set` member.
//!
//! SPEC.md Annex A.1 line 4400: "**map value**: `none` is the **key absent**. A
//! map never stores a `none` value; absence is the key not being present." The
//! set-element bullet just above (A.1, ~line 4398) and §5.5 line 489 ("the member
//! shape of a set is never `optional<T>`") say the same for set members: "`none`
//! is **not a member** … never a valid set element."
//!
//! `decode.rs::decode_map` and `decode.rs::decode_set` are the codec's last line
//! of defense: even though the model now REJECTS `map<K, optional<V>>` and
//! `set<optional<T>>` at build (see the sibling runtime reachability probe), an
//! untrusted wire / import / §19-restore / §20-migration payload could still carry
//! a `null` in a value/element position. The codec drops it — a `none` map value
//! leaves the key absent, a `none` set member is a non-member — so a `none` never
//! lands as a stored map value or set member. This test pins that drop.
//!
//! Every expected result is derived from A.1 alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;

use liasse_value::{Text, Type, Value};

fn opt(inner: Type) -> Type {
    Type::Optional(Box::new(inner))
}

// ===========================================================================
// REGRESSION: a `none` decoded for a map value / set member is DROPPED, never
// retained — the codec's defense against a wire/import `null` in those positions.
// ===========================================================================

#[test]
fn decode_map_authoring_must_not_store_a_none_value() {
    let ty = Type::Map(Box::new(Type::Text), Box::new(opt(Type::Text)));
    // Wire map: `a -> "x"` present, `b -> null` (a `none` map value).
    let wire = serde_json::json!([["a", "x"], ["b", null]]);
    let authored = ty.decode(&wire).expect("decode (authoring)");
    let Value::Map(entries) = &authored else { panic!("expected a map, got {authored:?}") };
    for (key, value) in entries {
        assert_ne!(
            *value,
            Value::None,
            "SPEC A.1 line 4400: a map never stores a `none` value, but {key:?} maps to `none`"
        );
    }
    assert_eq!(entries.len(), 1, "SPEC A.1 line 4400: the `b -> null` entry is absence, dropped");
}

#[test]
fn decode_wire_map_must_not_store_a_none_value() {
    let ty = Type::Map(Box::new(Type::Text), Box::new(opt(Type::Text)));
    let wire = serde_json::json!([["a", "x"], ["b", null]]);
    // Machine wire/request boundary (the §19 restore / untrusted peer path).
    let decoded = ty.decode_wire(&wire).expect("decode (wire)");
    let Value::Map(entries) = &decoded else { panic!("expected a map, got {decoded:?}") };
    assert!(
        entries.values().all(|v| *v != Value::None),
        "SPEC A.1 line 4400: no map value is ever `none`; got {entries:?}"
    );
}

#[test]
fn decode_set_optional_element_must_not_store_a_none_member() {
    // Co-finding: a set element type is likewise structurally `optional`-capable
    // and the model accepts `set<optional<T>>`; a `null` element decodes to a
    // `none` MEMBER, which A.1 (set-element bullet) / §5.5 line 489 forbid.
    let ty = Type::Set(Box::new(opt(Type::Text)));
    let wire = serde_json::json!(["x", null]);
    let decoded = ty.decode(&wire).expect("decode set");
    let Value::Set(members) = &decoded else { panic!("expected a set, got {decoded:?}") };
    assert!(
        !members.contains(&Value::None),
        "SPEC §5.5 line 489 / A.1: `none` is never a set member, but the decoded set holds it: {members:?}"
    );
}

// ===========================================================================
// PASSING CONTROLS: the codec handles `none` correctly where A.1 ALLOWS it, and
// drops nothing spurious — isolating the bug to the set/map inner positions.
// ===========================================================================

#[test]
fn control_present_map_and_set_round_trip_intact() {
    // A `map<text, text>` and a `set<text>` of present values keep every entry.
    let map_ty = Type::Map(Box::new(Type::Text), Box::new(Type::Text));
    let decoded = map_ty.decode(&serde_json::json!([["a", "x"], ["b", "y"]])).expect("map");
    let mut expected = BTreeMap::new();
    expected.insert(Value::Text(Text::new("a")), Value::Text(Text::new("x")));
    expected.insert(Value::Text(Text::new("b")), Value::Text(Text::new("y")));
    assert_eq!(decoded, Value::Map(expected), "present map entries survive");

    let set_ty = Type::Set(Box::new(Type::Text));
    let decoded = set_ty.decode(&serde_json::json!(["x", "y"])).expect("set");
    let Value::Set(members) = decoded else { panic!("set") };
    assert_eq!(members.len(), 2, "present set members survive");
}

#[test]
fn control_top_level_optional_null_decodes_to_none_correctly() {
    // The LEGITIMATE `none` position: a top-level `optional<text>` decodes a wire
    // `null` to `none` (A.1 — an optional member's `none`). This proves the codec
    // handles `none` correctly where A.1 places it, so the set/map retention above
    // is a positional bug, not a general `none`-handling defect.
    assert_eq!(opt(Type::Text).decode(&serde_json::json!(null)).expect("optional"), Value::None);
    assert_eq!(
        opt(Type::Text).decode(&serde_json::json!("x")).expect("optional present"),
        Value::Text(Text::new("x"))
    );
}
