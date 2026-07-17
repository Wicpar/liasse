#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 / §7.5: a `$view` whose result is a single scalar value — a bare root
//! scalar field (`.n`) or an aggregate (`= size(.things)`) — delivers that value,
//! not an empty row stream. A row-stream view still delivers its rows, and a
//! root-scalar mutation `return` yields the scalar value.

mod support;

use liasse_runtime::{CallRequest, Value, ViewResult};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

const SCALARS: &str = r#"{
  "$liasse": 1
  "$app": "t.scalarview@1.0.0"
  "$model": {
    "n": "int = 0"
    "plain": { "$view": ".n" }
    "doubled": { "$view": ".n + .n" }
    "things": {
      "$key": "id"
      "id": "text"
    }
    "count": { "$view": "size(.things)" }
    "all_things": { "$view": ".things { id }" }
    "$mut": {
      "add_thing": ".things + { id: @id }"
      "read_n": "return .n"
      "bump": [ ".n = .n + 1", "return .n" ]
    }
  }
  "$data": { "n": 7 }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn add_thing(engine: &mut liasse_runtime::Engine<MemoryStore>, id: &str) {
    let mut generator = generator();
    engine
        .call(&CallRequest::new("add_thing").arg("id", text(id)), &mut generator)
        .expect("call")
        .committed_at()
        .expect("add_thing commits");
}

fn scalar(engine: &liasse_runtime::Engine<MemoryStore>, view: &str) -> Value {
    match engine.view_at_head(view).expect("view").expect("declared") {
        ViewResult::Scalar(value) => value,
        ViewResult::Rows(rows) => panic!("view `{view}` delivered {} rows, expected a scalar", rows.len()),
    }
}

#[test]
fn a_bare_root_scalar_view_delivers_its_value() {
    let engine = load("scalarview", SCALARS);
    let result = engine.view_at_head("plain").expect("view").expect("declared");
    // A scalar result carries the value, exposes no rows, and reports empty.
    assert_eq!(result.scalar().map(Value::to_wire), Some(serde_json::json!("7")));
    assert!(result.rows().is_empty(), "a scalar view has no rows");
    assert!(result.is_empty(), "a scalar view reports no rows");
    // The value is the root field's stored int (canonical wire is a JSON string).
    assert_eq!(scalar(&engine, "plain").to_wire(), serde_json::json!("7"));
    assert_eq!(scalar(&engine, "doubled").to_wire(), serde_json::json!("14"));
}

#[test]
fn an_aggregate_view_delivers_its_scalar_count() {
    let mut engine = load("scalarview-agg", SCALARS);
    assert_eq!(scalar(&engine, "count").to_wire(), serde_json::json!("0"));
    add_thing(&mut engine, "a");
    assert_eq!(scalar(&engine, "count").to_wire(), serde_json::json!("1"), "the aggregate reflects the new row");
}

#[test]
fn a_row_stream_view_still_delivers_rows() {
    let mut engine = load("scalarview-rows", SCALARS);
    add_thing(&mut engine, "a");
    let result = engine.view_at_head("all_things").expect("view").expect("declared");
    assert!(result.scalar().is_none(), "a row-stream view is not scalar");
    assert_eq!(result.rows().len(), 1);
}

#[test]
fn a_root_scalar_mutation_return_yields_the_value() {
    let mut engine = load("scalarview-ret", SCALARS);
    let mut generator = generator();
    let outcome = engine.call(&CallRequest::new("read_n"), &mut generator).expect("call ok");
    let response = outcome.response().expect("a scalar return delivers a response value");
    assert_eq!(response.to_wire(), serde_json::json!("7"), "a root-scalar `return` yields the value");
}

const NODATA: &str = r#"{
  "$liasse": 1
  "$app": "t.nodata@1.0.0"
  "$model": {
    "n": "int = 0"
    "plain": { "$view": ".n" }
    "$mut": { "bump": [ ".n = .n + 1", "return .n" ] }
  }
}"#;

#[test]
fn a_root_singleton_field_takes_its_default_at_genesis() {
    // §8.2: with no `$data`, the root field `n` takes its declared default 0, so a
    // view reads 0 and `bump` (`.n = .n + 1`) commits and returns 1 rather than
    // faulting on an absent value.
    let mut engine = load("nodata", NODATA);
    let mut generator = generator();
    match engine.view_at_head("plain").expect("view").expect("declared") {
        ViewResult::Scalar(value) => assert_eq!(value.to_wire(), serde_json::json!("0"), "default applied"),
        ViewResult::Rows(_) => panic!("plain is a scalar view"),
    }
    let outcome = engine.call(&CallRequest::new("bump"), &mut generator).expect("call ok");
    assert_eq!(
        outcome.response().expect("bump returns a value").to_wire(),
        serde_json::json!("1"),
        "bump increments the defaulted root scalar",
    );
}

#[test]
fn a_root_singleton_write_then_scalar_return_yields_the_new_value() {
    // §8.2/§8.10: writing a root singleton field then returning it delivers the
    // committed scalar (`bump` returns the incremented `.n`), not `none`.
    let mut engine = load("scalarview-bump", SCALARS);
    let mut generator = generator();
    let outcome = engine.call(&CallRequest::new("bump"), &mut generator).expect("call ok");
    let response = outcome.response().expect("bump delivers a response value");
    assert_eq!(response.to_wire(), serde_json::json!("8"), "bump returns the incremented root scalar");
    // The scalar view reflects the committed write.
    assert_eq!(scalar(&engine, "plain").to_wire(), serde_json::json!("8"));
}
