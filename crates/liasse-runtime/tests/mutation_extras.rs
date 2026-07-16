#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Mutation-interpreter behaviours beyond the single-row CORE forms:
//! omitted-optional parameters (§8.3), filtered bulk patches (§8.9), and the
//! delete-and-capture local binding (§8.4). Each expectation is re-derived from
//! SPEC.md, not from the implementation's own output.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Value};
use liasse_value::Text;
use support::{generator, load};

type Eng = liasse_runtime::Engine<liasse_store::MemoryStore>;

fn call(engine: &mut Eng, mutation: &str, args: &[(&str, &str)]) -> CallOutcome {
    let mut generator = generator();
    let mut request = CallRequest::new(mutation);
    for (name, value) in args {
        request = request.arg(*name, Value::Text(Text::new(*value)));
    }
    engine.call(&request, &mut generator).expect("the call reaches admission")
}

// ---- §8.3: an omitted optional parameter binds `none` -----------------------

const PROFILES: &str = r#"{
  "$liasse": 1
  "$app": "t.optparam@1.0.0"
  "$model": {
    "profiles": { "$key": "id", "id": "text", "email": "text?" }
    "all": { "$view": ".profiles { id, email }" }
    "$mut": { "set_email": ".profiles[@id] { email = @email }" }
  }
  "$data": { "profiles": { "p1": { "email": "a@x" } } }
}"#;

#[test]
fn omitted_optional_param_clears_the_field() {
    let mut engine = load("optparam", PROFILES);
    // §8.3/§A.1: `@email` omitted binds `none`; §8.5 assigning `none` clears it.
    let outcome = call(&mut engine, "set_email", &[("id", "p1")]);
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "clearing an optional field commits: {outcome:?}");
    let view = engine.view_at_head("all").expect("view").expect("declared");
    assert_eq!(view.rows()[0].field("email"), None, "the optional field reads as absent after clearing");
}

#[test]
fn supplied_optional_param_sets_the_field() {
    let mut engine = load("optparam2", PROFILES);
    call(&mut engine, "set_email", &[("id", "p1"), ("email", "b@y")]);
    let view = engine.view_at_head("all").expect("view").expect("declared");
    assert_eq!(view.rows()[0].field("email").map(Value::to_wire), Some(serde_json::json!("b@y")));
}

// ---- §8.9: a filtered bulk patch --------------------------------------------

const BULK: &str = r#"{
  "$liasse": 1
  "$app": "t.bulkpatch@1.0.0"
  "$model": {
    "tasks": { "$key": "id", "id": "text", "done": "bool = false", "archived": "bool = false" }
    "archived_view": { "$view": ".tasks[:t | t.archived] { id }" }
    "$mut": {
      "mark_done": ".tasks[@id] { done = true }"
      "archive_done": ".tasks[:t | t.done] { archived = true }"
    }
  }
  "$data": { "tasks": { "t1": {}, "t2": {} } }
}"#;

#[test]
fn zero_match_bulk_patch_is_unchanged() {
    let mut engine = load("bulk0", BULK);
    // No task is done: the filtered patch selects zero rows and stages nothing.
    let outcome = call(&mut engine, "archive_done", &[]);
    assert!(matches!(outcome, CallOutcome::Unchanged { .. }), "a zero-match bulk patch is unchanged: {outcome:?}");
}

#[test]
fn bulk_patch_updates_every_matched_row() {
    let mut engine = load("bulkN", BULK);
    call(&mut engine, "mark_done", &[("id", "t1")]);
    call(&mut engine, "mark_done", &[("id", "t2")]);
    let outcome = call(&mut engine, "archive_done", &[]);
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "patching matched rows commits: {outcome:?}");
    let view = engine.view_at_head("archived_view").expect("view").expect("declared");
    assert_eq!(view.len(), 2, "both done tasks are archived by the one bulk patch");
}

// ---- §8.4: delete-and-capture local binding ---------------------------------

const NOTES: &str = r#"{
  "$liasse": 1
  "$app": "t.delcap@1.0.0"
  "$model": {
    "notes": { "$key": "id", "id": "text", "body": "text" }
    "all": { "$view": ".notes { id, body, $sort: [id] }" }
    "$mut": {
      "put": [ "n = .notes + { id: @id, body: @body }", "return n { id, body }" ]
      "remove_pair": [ "removed = -.notes[@x, @y, @x]", "return removed { id }" ]
    }
  }
  "$data": { "notes": { "n1": { "body": "one" }, "n2": { "body": "two" } } }
}"#;

#[test]
fn delete_capture_returns_removed_rows_in_selector_order_deduplicated() {
    let mut engine = load("delcap", NOTES);
    // §8.4: the deleted rows, in selector order, first occurrence of a duplicate
    // key kept — `[n2, n1, n2]` captures `[n2, n1]` as they existed before delete.
    let outcome = call(&mut engine, "remove_pair", &[("x", "n2"), ("y", "n1")]);
    let wire = outcome.response().expect("the capture is returned").to_wire();
    let rows = wire.as_array().expect("§8.4: a delete returns a view (array), never a bare row");
    let ids: Vec<_> = rows.iter().map(|r| r["id"].as_str().unwrap().to_owned()).collect();
    assert_eq!(ids, vec!["n2".to_owned(), "n1".to_owned()], "captured in selector order, deduplicated");
    let view = engine.view_at_head("all").expect("view").expect("declared");
    assert_eq!(view.len(), 0, "both rows are removed from live state");
}
