#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM regression — a `map` field never surfaces a `none` (A.1 line 4400)
//! and an omitted non-optional `map` field defaults to the EMPTY map (§5.5, the
//! set-analogous pin; SPEC-ISSUES item 37).
//!
//! SPEC.md Annex A.1 (line 4396): "`none` is absence in the Liasse type system,
//! not a value: it cannot be a member of a set, a **map value**, or a distinct
//! thing carried by a wire marker." Line 4400: "**map value**: `none` is the
//! **key absent**. A map never stores a `none` value." The clean enforcement is a
//! STATIC error: a `map<K, optional<V>>` field cannot be declared, so no `none`
//! ever reaches a map value (verified at the runtime boundary here, and unit-wise
//! in redteam_optional_container_inner_reachability).
//!
//! §5.5 pins an omitted set / keyed collection to EMPTY. It was silent on a `map`
//! value-typed field, which defaulted to `Value::None` — projecting as absence and
//! sitting uneasily with §22.1 (the declared shape holds in every committed state).
//! The fix pins it the set-analogous way: an omitted non-optional `map` field is
//! the empty map. This test asserts that fixed behavior.
//!
//! These are pure `liasse-runtime` reproductions over `MemoryStore`. Expectations
//! are derived from SPEC.md alone, never echoed from the runtime.

mod support;

use std::collections::BTreeMap;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load, store};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut generator = generator();
    engine.call(request, &mut generator).expect("call runs")
}

/// The raw stored/projected `meta` field of the single row.
fn meta_field(engine: &Engine<MemoryStore>, view: &str) -> Option<Value> {
    let result = engine.view_at_head(view).expect("view").expect("declared view");
    result.rows()[0].field("meta").cloned()
}

// ---------------------------------------------------------------------------
// A `map<K, optional<V>>` field cannot be declared: the map-value `none` path is
// closed at the type level (A.1 line 4400).
// ---------------------------------------------------------------------------

const OPT_VALUE_MAP: &str = r#"{
  "$liasse": 1,
  "$app": "redteam.mapnone.optval@1.0.0",
  "$model": {
    "docs": {
      "$key": "id",
      "id": "text",
      "meta": "map<text, optional<text>>"
    }
  }
}"#;

#[test]
fn optional_map_value_type_is_rejected_at_load() {
    // A.1 line 4400: a map never stores a `none` value; the value type is never
    // `optional<V>`, so this model is a static error at the runtime boundary. (The
    // rustc-like diagnostic wording is asserted at the model layer, where the
    // message is accessible — see liasse-model `redteam_optional_container_inner`;
    // `Engine::load` surfaces only a summary error.)
    let mut generator = generator();
    let result = Engine::load(store("mapnone-optval"), OPT_VALUE_MAP, &mut generator);
    assert!(
        result.is_err(),
        "A.1 line 4400: `map<text, optional<text>>` must be rejected at load, but it succeeded"
    );
}

// ---------------------------------------------------------------------------
// A valid `map<text, text>` field: present values round-trip, and an OMITTED
// field defaults to the empty map (not `none`).
// ---------------------------------------------------------------------------

const TEXT_VALUE_MAP: &str = r#"{
  "$liasse": 1,
  "$app": "redteam.mapnone.textval@1.0.0",
  "$model": {
    "docs": {
      "$key": "id",
      "id": "text",
      "meta": "map<text, text>"
    },
    "docs_view": { "$view": ".docs { id, meta }" },
    "$mut": {
      "add({ id: text })": ".docs + { id: @id }",
      "add_full({ id: text, meta: map<text, text> })": ".docs + { id: @id, meta: @meta }"
    }
  }
}"#;

#[test]
fn present_map_values_round_trip() {
    let mut engine = load("mapnone-present", TEXT_VALUE_MAP);
    let mut m = BTreeMap::new();
    m.insert(text("a"), text("x"));
    m.insert(text("b"), text("y"));
    let outcome = call(
        &mut engine,
        &CallRequest::new("add_full").arg("id", text("d1")).arg("meta", Value::Map(m.clone())),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "insert commits: {outcome:?}");
    assert_eq!(
        meta_field(&engine, "docs_view"),
        Some(Value::Map(m)),
        "present entries survive intact"
    );
}

#[test]
fn omitted_non_optional_map_field_defaults_to_empty() {
    let mut engine = load("mapnone-omitted", TEXT_VALUE_MAP);
    let outcome = call(&mut engine, &CallRequest::new("add").arg("id", text("d1")));
    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "insert omitting the map field commits: {outcome:?}"
    );

    // §5.5 / SPEC-ISSUES item 37: an omitted non-optional `map` field is the EMPTY
    // map — the set-analogous default — NOT `none`. Before the fix this projected
    // as absence (`None`), breaching §22.1 (the declared `map` shape must hold in
    // every committed state) and diverging from the set default.
    assert_eq!(
        meta_field(&engine, "docs_view"),
        Some(Value::Map(BTreeMap::new())),
        "SPEC §5.5: an omitted non-optional `map` field defaults to the empty map, not `none`"
    );
}
