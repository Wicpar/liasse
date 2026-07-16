#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.2 root singleton state: a package root's non-collection writable members —
//! scalar fields and static (possibly nested) structs declared directly under
//! `$model` — are durable state seeded from `$data`, stored, and materialized
//! onto the package root so a view reads `.field` and `.struct.member`.

mod support;

use support::load;

const SINGLETONS: &str = r#"{
  "$liasse": 1
  "$app": "t.singletons@1.0.0"
  "$model": {
    "t": "text"
    "company": {
      "name": "text"
      "address": { "city": "text", "country": "text" }
    }
    "readout": {
      "$view": ". { t, cname: .company.name, city: .company.address.city }"
    }
  }
  "$data": {
    "t": "  Read  the spec  "
    "company": { "name": "Acme", "address": { "city": "Paris", "country": "FR" } }
  }
}"#;

#[test]
fn root_scalar_and_nested_struct_materialize_for_a_view() {
    let engine = load("singletons", SINGLETONS);
    let view = engine.view_at_head("readout").expect("view").expect("declared");
    assert_eq!(view.len(), 1, "the root projects one row");
    let row = &view.rows()[0];
    // §A.1: a `text` value is preserved exactly — no implicit trimming.
    assert_eq!(row.field("t").map(liasse_value::Value::to_wire), Some(serde_json::json!("  Read  the spec  ")));
    // The root struct and its nested struct resolve field-by-field.
    assert_eq!(row.field("cname").map(liasse_value::Value::to_wire), Some(serde_json::json!("Acme")));
    assert_eq!(row.field("city").map(liasse_value::Value::to_wire), Some(serde_json::json!("Paris")));
}
