#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.5 set-field mutations (`.tags + m` union, `.tags - m` difference) and
//! §8.9 no-change completion, driven through a row mutation. Each expectation is
//! re-derived from §8.5/§8.9: adding an existing member or removing an absent one
//! succeeds without changing state; a genuine change commits. Set read order is
//! the element type's canonical order (§5.5, Annex B.1).

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::{Text, Value as V};
use support::{generator, load};

const DOCS: &str = r#"{
  "$liasse": 1,
  "$app": "example.setfields@1.0.0",
  "$model": {
    "docs": {
      "$key": "id",
      "id": "text",
      "tags": { "$set": "text" },
      "$mut": { "tag": ".tags + @tag", "untag": ".tags - @tag" }
    },
    "docs_view": { "$view": ".docs { id, tags }" },
    "$mut": { "add_doc": ".docs + { id: @id }" }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut generator = generator();
    engine.call(request, &mut generator).expect("call")
}

fn tags(engine: &Engine<MemoryStore>) -> Vec<Value> {
    let view = engine.view_at_head("docs_view").expect("view").expect("declared");
    match view.rows()[0].field("tags") {
        Some(V::Set(members)) => members.iter().cloned().collect(),
        other => panic!("tags is a set, got {other:?}"),
    }
}

#[test]
fn omitted_set_starts_empty() {
    // §5.5: when a row is created, an omitted set starts empty — an empty set,
    // not an absent optional. It projects as an empty set (wire `[]`) rather
    // than a missing member.
    let mut engine = load("setfields", DOCS);
    assert!(matches!(
        call(&mut engine, &CallRequest::new("add_doc").arg("id", text("d1"))),
        CallOutcome::Committed { .. }
    ));
    let view = engine.view_at_head("docs_view").expect("view").expect("declared");
    match view.rows()[0].field("tags") {
        Some(V::Set(members)) => assert!(members.is_empty(), "an omitted set starts empty"),
        other => panic!("an omitted set is an empty set, not {other:?}"),
    }
}

#[test]
fn set_add_and_remove_apply_and_noop() {
    let mut engine = load("setfields", DOCS);
    assert!(matches!(
        call(&mut engine, &CallRequest::new("add_doc").arg("id", text("d1"))),
        CallOutcome::Committed { .. }
    ));

    // First tag: a genuine addition commits.
    assert!(matches!(
        call(&mut engine, &CallRequest::new("tag").receiver(text("d1")).arg("tag", text("a"))),
        CallOutcome::Committed { .. }
    ));
    // §8.5/§8.9: re-adding an existing member changes nothing → unchanged.
    assert!(matches!(
        call(&mut engine, &CallRequest::new("tag").receiver(text("d1")).arg("tag", text("a"))),
        CallOutcome::Unchanged { .. }
    ));
    // Removing an absent member changes nothing → unchanged.
    assert!(matches!(
        call(&mut engine, &CallRequest::new("untag").receiver(text("d1")).arg("tag", text("z"))),
        CallOutcome::Unchanged { .. }
    ));
    // A second distinct member commits; canonical order is "a" < "b" (B.1).
    assert!(matches!(
        call(&mut engine, &CallRequest::new("tag").receiver(text("d1")).arg("tag", text("b"))),
        CallOutcome::Committed { .. }
    ));
    assert_eq!(tags(&engine), vec![text("a"), text("b")]);

    // Removing a present member commits and drops it from the set.
    assert!(matches!(
        call(&mut engine, &CallRequest::new("untag").receiver(text("d1")).arg("tag", text("a"))),
        CallOutcome::Committed { .. }
    ));
    assert_eq!(tags(&engine), vec![text("b")]);
}
