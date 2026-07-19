#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.1 lexical local bindings in a mutation program: the idiomatic
//! insert-and-return form `t = .coll + { .. }` then `return t { .. }` must
//! stage the constructed row (§8.4) and evaluate the response — including the
//! row's generated key — from the committed resulting state (§8.10, §8.12).

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Value};
use liasse_value::Text;
use support::{generator, load};

/// A minimal app whose sole mutation binds an inserted row to a local and
/// returns a projection of it — the pattern §8 documents and the corpus uses.
const LOCALS: &str = r#"{
  "$liasse": 1
  "$app": "example.locals@1.0.0"
  "$model": {
    "tasks": {
      "$key": "id"
      "id": "uuid = uuid()"
      "title": "text"
      "done": "bool = false"
    }
    "all_tasks": { "$view": ".tasks { id, title, done }" }
    "$mut": {
      "add_task": [
        "t = .tasks + { title: @title }"
        "return t { id, title, done }"
      ]
    }
  }
}"#;

fn add_task(engine: &mut liasse_runtime::Engine<liasse_store::MemoryStore>, title: &str) -> CallOutcome {
    let mut generator = generator();
    engine
        .call(&CallRequest::new("add_task").arg("title", Value::Text(Text::new(title))), &mut generator)
        .expect("call")
}

#[test]
fn local_binding_insert_stages_the_row_and_commits() {
    let mut engine = load("locals", LOCALS);
    let head = engine.head().unwrap();
    let outcome = add_task(&mut engine, "Ship the spec");
    // §8.4: the insert stages a row, so the call commits (not `unchanged`).
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the insert must commit a new row");
    assert_ne!(engine.head().unwrap(), head, "a committed insert advances the frontier");
    let view = engine.view_at_head("all_tasks").expect("view").expect("declared");
    assert_eq!(view.len(), 1, "exactly the one inserted row is committed");
}

#[test]
fn returned_row_matches_committed_state_including_generated_key() {
    let mut engine = load("locals", LOCALS);
    let outcome = add_task(&mut engine, "Ship the spec");
    let response = outcome.response().expect("a return value").to_wire();

    // The return projected the bound row; its fields must equal the committed row.
    let view = engine.view_at_head("all_tasks").expect("view").expect("declared");
    let row = &view.rows()[0];
    let committed_id = row.field("id").expect("committed id").to_wire();

    assert_eq!(response["title"], serde_json::json!("Ship the spec"));
    assert_eq!(response["done"], serde_json::json!(false), "the `done` default applied");
    assert_eq!(
        response["id"], committed_id,
        "§8.10/§8.12: the returned generated id is the one committed for the row"
    );
}

#[test]
fn each_call_binds_and_returns_a_distinct_row() {
    let mut engine = load("locals", LOCALS);
    // One generator across both calls so successive requests draw distinct
    // seeds (each admission consumes one), as a live driver would.
    let mut generator = generator();
    let call = |engine: &mut liasse_runtime::Engine<liasse_store::MemoryStore>,
                generator: &mut liasse_runtime::FixedGenerators,
                title: &str| {
        engine
            .call(&CallRequest::new("add_task").arg("title", Value::Text(Text::new(title))), generator)
            .expect("call")
    };
    let first = call(&mut engine, &mut generator, "one").response().expect("value").to_wire();
    let second = call(&mut engine, &mut generator, "two").response().expect("value").to_wire();
    assert_ne!(first["id"], second["id"], "two inserts produce two distinct generated keys");
    let view = engine.view_at_head("all_tasks").expect("view").expect("declared");
    assert_eq!(view.len(), 2, "both inserted rows are committed");
}
