#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! The `map` collection (SPEC.md §5.4): a declaration carrying `$value` beside
//! `$key` is the degenerate keyed collection, whose row shape is the fixed
//! `{ $key, $value }`.
//!
//! Everything asserted here is derived from §5.4 rather than from the runtime's
//! own answer:
//!
//! - `$value` is what puts a declaration in map form, and only then is `$key` a
//!   TYPE expression. Without `$value` the very same object is a table and `$key`
//!   names a declared field — so the two readings must not bleed into each other.
//! - "A map entry is a real row … one row per entry in storage, one delta per
//!   changed entry": entries are inserted, replaced, and deleted one at a time
//!   through the ordinary row surface.
//! - "A map IS a table, so every access a keyed collection has applies to it
//!   unchanged": the key selector, filters, `count`, and projection all work with
//!   no map-specific spelling.
//! - "`m[k]` … contributes zero rows when the key is absent" (§6.3, verbatim) —
//!   never a default.
//! - `m { $key }` / `m { $value }` are the two whole-collection projections.
//!
//! Pure `liasse-runtime` reproductions over `MemoryStore`.

mod support;

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

fn committed(outcome: &CallOutcome, what: &str) {
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "{what}: {outcome:?}");
}

/// One scalar view result.
fn scalar(engine: &Engine<MemoryStore>, view: &str) -> Value {
    let result = engine.view_at_head(view).expect("view").expect("declared view");
    result.scalar().cloned().unwrap_or_else(|| panic!("`{view}` is not a scalar view: {result:?}"))
}

/// The `$key` values of a view's rows, in read order.
fn keys(engine: &Engine<MemoryStore>, view: &str) -> Vec<Value> {
    let result = engine.view_at_head(view).expect("view").expect("declared view");
    result.rows().iter().filter_map(|row| row.field("k").cloned()).collect()
}

/// A map of `text` settings plus the views and mutations that exercise it.
const SETTINGS: &str = r#"{
  "$liasse": 1
  "$app": "t.map.settings@1.0.0"
  "$model": {
    "settings": { "$key": "text", "$value": "text" }
    "entries": { "$view": ".settings[:e] { k: e.$key, v: e.$value }" }
    "just_keys": { "$view": ".settings { $key }" }
    "just_values": { "$view": ".settings { $value }" }
    "size": { "$view": "= count(.settings)" }
    "$mut": {
      "put({ name: text, value: text })": ".settings + { $key: @name, $value: @value }"
    }
  }
}"#;

fn seeded(instance: &str) -> Engine<MemoryStore> {
    let mut engine = load(instance, SETTINGS);
    for (name, value) in [("theme", "dark"), ("locale", "fr"), ("tz", "Europe/Paris")] {
        committed(
            &call(
                &mut engine,
                &CallRequest::new("put").arg("name", text(name)).arg("value", text(value)),
            ),
            "an entry insert commits",
        );
    }
    engine
}

#[test]
fn entries_are_rows_addressed_one_at_a_time() {
    // §5.4: "A map entry is a real row … Entries are addressable, writable,
    // deletable … one row per entry in storage." Three inserts give three rows,
    // each carrying its own `{ $key, $value }`.
    let engine = seeded("map-entries");
    assert_eq!(scalar(&engine, "size"), Value::Int(3.into()), "count(.settings) counts entries");
    assert_eq!(
        keys(&engine, "entries"),
        vec![text("locale"), text("theme"), text("tz")],
        "§5.4: a map's read order is the total order of its key type (Annex B)"
    );
}

#[test]
fn the_two_whole_collection_projections_read_the_two_columns() {
    // §5.4: "`m { $key }` and `m { $value }` … each collapses the entry stream to
    // the set of that column's values, deduplicated in the element type's
    // canonical order like any set."
    let engine = seeded("map-columns");
    let expected_keys: std::collections::BTreeSet<Value> =
        [text("locale"), text("theme"), text("tz")].into_iter().collect();
    assert_eq!(
        scalar(&engine, "just_keys"),
        Value::Set(expected_keys),
        "`{{ $key }}` is the set of entry keys"
    );
    let expected_values: std::collections::BTreeSet<Value> =
        [text("dark"), text("fr"), text("Europe/Paris")].into_iter().collect();
    assert_eq!(
        scalar(&engine, "just_values"),
        Value::Set(expected_values),
        "`{{ $value }}` is the set of entry values"
    );
}

/// The absent-key and per-entry-mutation surface, kept apart from `SETTINGS` so
/// each view names exactly one §5.4 claim.
const ADDRESSING: &str = r#"{
  "$liasse": 1
  "$app": "t.map.addressing@1.0.0"
  "$model": {
    "settings": { "$key": "text", "$value": "text" }
    "hit": { "$view": ".settings[\"theme\"] { k: .$key, v: .$value }" }
    "miss": { "$view": ".settings[\"absent\"] { k: .$key, v: .$value }" }
    "$mut": {
      "put({ name: text, value: text })": ".settings + { $key: @name, $value: @value }"
      "retitle({ name: text, value: text })": ".settings[@name].$value = @value"
      "drop({ name: text })": ".settings - [@name]"
    }
  }
}"#;

#[test]
fn an_absent_key_selects_zero_rows_and_never_a_default() {
    // §5.4 quoting §6.3 verbatim: "one scalar or composite key contributes zero
    // rows when the key is absent and one row when it exists." A map never
    // substitutes a default, a sentinel, or an empty value for a missing entry.
    let mut engine = load("map-absent", ADDRESSING);
    committed(
        &call(
            &mut engine,
            &CallRequest::new("put").arg("name", text("theme")).arg("value", text("dark")),
        ),
        "an entry insert commits",
    );
    assert_eq!(keys(&engine, "hit"), vec![text("theme")], "a present key selects its one entry");
    assert!(
        keys(&engine, "miss").is_empty(),
        "§6.3: an absent key contributes ZERO rows — not a `none`, not an empty value"
    );
}

#[test]
fn an_entry_is_replaced_and_deleted_on_its_own() {
    // §5.4: "Entries are addressable, writable, deletable … one delta per changed
    // entry." Writing one entry's `$value` leaves its siblings alone, and removing
    // one entry removes exactly that row.
    let mut engine = load("map-per-entry", ADDRESSING);
    for (name, value) in [("theme", "dark"), ("locale", "fr")] {
        committed(
            &call(
                &mut engine,
                &CallRequest::new("put").arg("name", text(name)).arg("value", text(value)),
            ),
            "an entry insert commits",
        );
    }
    committed(
        &call(
            &mut engine,
            &CallRequest::new("retitle").arg("name", text("theme")).arg("value", text("light")),
        ),
        "writing one entry's value commits",
    );
    let result = engine.view_at_head("hit").expect("view").expect("declared view");
    assert_eq!(
        result.rows()[0].field("v"),
        Some(&text("light")),
        "the addressed entry carries its new value"
    );

    committed(
        &call(&mut engine, &CallRequest::new("drop").arg("name", text("theme"))),
        "removing one entry commits",
    );
    assert!(
        keys(&engine, "hit").is_empty(),
        "§5.4: the removed entry is gone; deletion is per entry"
    );
}

#[test]
fn value_without_key_is_a_static_error() {
    // §5.4: "A `$value` with no `$key` is likewise a static error: a map's entries
    // are keyed, so the key type is not optional."
    let package = r#"{
      "$liasse": 1, "$app": "t.map.novalue@1.0.0",
      "$model": { "settings": { "$value": "text" } }
    }"#;
    let mut generator = support::generator();
    assert!(
        Engine::load(store("map-no-key"), package, &mut generator).is_err(),
        "§5.4: `$value` needs a `$key`"
    );
}

#[test]
fn value_beside_a_mutually_exclusive_marker_is_a_static_error() {
    // §5.4 / Annex C.2: "`$value` composes only with `$key`"; beside a
    // mutually-exclusive kind marker the node kind is undetermined, which is a
    // static error naming both — never a marker silently winning.
    let package = r#"{
      "$liasse": 1, "$app": "t.map.conflict@1.0.0",
      "$model": { "settings": { "$key": "text", "$value": "text", "$set": "text" } }
    }"#;
    let mut generator = support::generator();
    assert!(
        Engine::load(store("map-conflict"), package, &mut generator).is_err(),
        "§5.4: `$value` beside `$set` is a static error"
    );
}

#[test]
fn without_value_the_same_object_is_still_a_table() {
    // §5.4: "Without `$value` the declaration is an ordinary table and `$key`
    // names declared fields." The map reading must not leak into a table: a
    // `$key` naming an undeclared field stays the error it always was.
    let table = r#"{
      "$liasse": 1, "$app": "t.map.table@1.0.0",
      "$model": { "docs": { "$key": "id", "id": "text", "title": "text" } }
    }"#;
    let mut generator = support::generator();
    assert!(
        Engine::load(store("map-table-ok"), table, &mut generator).is_ok(),
        "a `$key`-only declaration is a table keyed on a declared field"
    );

    let undeclared = r#"{
      "$liasse": 1, "$app": "t.map.tablebad@1.0.0",
      "$model": { "docs": { "$key": "text", "id": "text" } }
    }"#;
    let mut generator = support::generator();
    assert!(
        Engine::load(store("map-table-bad"), undeclared, &mut generator).is_err(),
        "§5.4: without `$value`, `$key` names a declared field — `text` is not one"
    );
}
