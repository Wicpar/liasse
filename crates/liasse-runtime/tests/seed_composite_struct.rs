#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Seed admission of composite-key rows (§5.4, D.2) and static-struct members
//! (§5.3), read back through a view so the ordering rules of Annex B apply.
//!
//! Every expected value is re-derived from the spec text, not the engine's own
//! answer:
//!
//! - A composite `$key` seed member name joins one D.2-escaped component per key
//!   field in `$key` order (D.2). When a row body omits the key fields, each key
//!   field must take its *own* decoded component — never the whole joined text.
//! - A static-struct field (§5.3) seeded from an object initializer stores a
//!   `struct` value whose members read back individually; a `$sort` over the
//!   struct compares its fields in canonical field-name (text) order (§B.4).

mod support;

use liasse_runtime::{Engine, Value, ViewQuery};
use support::{generator, store};

fn engine(tag: &str, app: &str) -> Engine<liasse_store::MemoryStore> {
    let mut generator = generator();
    Engine::load(store(tag), app, &mut generator).unwrap_or_else(|error| panic!("load failed: {error}"))
}

fn text_field(row: &liasse_runtime::ViewRow, name: &str) -> Option<String> {
    match row.field(name) {
        Some(Value::Text(t)) => Some(t.as_str().to_owned()),
        _ => None,
    }
}

/// §5.4/D.2: with `$key: [region, city]` and seed members `"amer:Lima"` etc. and
/// empty bodies, each row's `region`/`city` field takes its own key component,
/// not the whole joined key. Default order is key-ascending (§B.5), comparing
/// `region` then `city`, each in text order (§B.1/§B.4).
#[test]
fn composite_key_seed_splits_components_and_orders_by_key() {
    const APP: &str = r#"{
      "$liasse": 1,
      "$app": "t.compkey@1.0.0",
      "$model": {
        "offices": { "$key": ["region", "city"], "region": "text", "city": "text" },
        "$public": { "all": { "$view": ".offices { region, city }" } }
      },
      "$data": { "offices": { "emea:Paris": {}, "amer:Lima": {}, "emea:Berlin": {}, "amer:Quito": {} } }
    }"#;
    let engine = engine("compkey", APP);
    let result = engine
        .view_with("public.all", engine.head(), &ViewQuery::new())
        .expect("view ok")
        .expect("view declared");
    let rows: Vec<(Option<String>, Option<String>)> = result
        .rows()
        .iter()
        .map(|row| (text_field(row, "region"), text_field(row, "city")))
        .collect();
    assert_eq!(
        rows,
        vec![
            (Some("amer".to_owned()), Some("Lima".to_owned())),
            (Some("amer".to_owned()), Some("Quito".to_owned())),
            (Some("emea".to_owned()), Some("Berlin".to_owned())),
            (Some("emea".to_owned()), Some("Paris".to_owned())),
        ]
    );
}

/// §5.3/§B.4: a static struct `pt` declared `(b, a)` is seeded from an object
/// initializer and sorted by `$sort: [pt]`. §B.4 compares struct fields in
/// canonical field-name (text) order, so `a` is the primary component: rows
/// order by `a` ascending. p2.a = 1 < p1.a = 2, so p2 precedes p1 — declaration
/// order (`b` first) would wrongly yield p1, p2.
#[test]
fn struct_seed_sorts_by_field_name_text_order() {
    const APP: &str = r#"{
      "$liasse": 1,
      "$app": "t.structsort@1.0.0",
      "$model": {
        "rows": { "$key": "id", "id": "text", "pt": { "b": "int", "a": "int" } },
        "$public": { "by_pt": { "$view": ".rows { id, $sort: [pt] }" } }
      },
      "$data": { "rows": { "p1": { "pt": { "b": 1, "a": 2 } }, "p2": { "pt": { "b": 2, "a": 1 } } } }
    }"#;
    let engine = engine("structsort", APP);
    let result = engine
        .view_with("public.by_pt", engine.head(), &ViewQuery::new())
        .expect("view ok")
        .expect("view declared");
    let ids: Vec<Option<String>> = result.rows().iter().map(|row| text_field(row, "id")).collect();
    assert_eq!(ids, vec![Some("p2".to_owned()), Some("p1".to_owned())]);
}
