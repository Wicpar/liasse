#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §5.9 enum admission: the `$enum` array is a closed label set, so a supplied
//! value outside it rejects the transition, while a declared label is admitted
//! and carried as a positioned enum value. Verified on both the insert form
//! (`.coll + { field: @arg }`) and the field-assignment form (`.field = @arg`),
//! since both reach the runtime through different admission paths.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Value};
use liasse_value::Text;
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

const ENUMS: &str = r#"{
  "$liasse": 1
  "$app": "example.enums@1.0.0"
  "$model": {
    "things": {
      "$key": "id"
      "id": "text"
      "status": { "$enum": ["draft", "active", "closed"] }
    }
    "all_things": { "$view": ".things { id, status }" }
    "$mut": {
      "add": ".things + { id: @id, status: @status }"
      "set_status": ".things[@id].status = @status"
    }
  }
}"#;

fn call(engine: &mut liasse_runtime::Engine<liasse_store::MemoryStore>, request: CallRequest) -> CallOutcome {
    let mut gens = generator();
    engine.call(&request, &mut gens).expect("call resolves")
}

/// §5.9: inserting a row whose enum field carries an undeclared label rejects the
/// whole transition; a declared label is admitted and visible in a view.
#[test]
fn insert_rejects_undeclared_enum_label() {
    let mut engine = load("enum-insert", ENUMS);

    let bad = CallRequest::new("add").arg("id", text("t1")).arg("status", text("archived"));
    assert!(matches!(call(&mut engine, bad), CallOutcome::Rejected(_)), "an undeclared label rejects");

    // The rejected insert committed nothing.
    let view = engine.view_at_head("all_things").expect("view").expect("declared");
    assert!(view.is_empty(), "the rejected transition left no row");

    let good = CallRequest::new("add").arg("id", text("t1")).arg("status", text("draft"));
    assert!(matches!(call(&mut engine, good), CallOutcome::Committed { .. }), "a declared label admits");

    let view = engine.view_at_head("all_things").expect("view").expect("declared");
    assert_eq!(view.len(), 1, "the admitted row is present");
    let status = view.rows()[0].fields().find(|(name, _)| name.as_str() == "status").expect("status field").1;
    assert_eq!(status.to_wire(), serde_json::json!("draft"), "the declared label is stored");
}

/// §5.9: assigning an undeclared label to an enum field rejects; a declared label
/// is admitted through the field-assignment path.
#[test]
fn assign_rejects_undeclared_enum_label() {
    let mut engine = load("enum-assign", ENUMS);
    let seed = CallRequest::new("add").arg("id", text("t1")).arg("status", text("draft"));
    assert!(matches!(call(&mut engine, seed), CallOutcome::Committed { .. }), "seed row admits");

    let bad = CallRequest::new("set_status").arg("id", text("t1")).arg("status", text("archived"));
    assert!(matches!(call(&mut engine, bad), CallOutcome::Rejected(_)), "an undeclared label rejects");

    let good = CallRequest::new("set_status").arg("id", text("t1")).arg("status", text("closed"));
    assert!(matches!(call(&mut engine, good), CallOutcome::Committed { .. }), "a declared label admits");

    let view = engine.view_at_head("all_things").expect("view").expect("declared");
    let status = view.rows()[0].fields().find(|(name, _)| name.as_str() == "status").expect("status field").1;
    assert_eq!(status.to_wire(), serde_json::json!("closed"), "the reassigned label is stored");
}
