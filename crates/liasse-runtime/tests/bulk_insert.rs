#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.7 insert-from-a-view and §5.1 batch semantics: one bulk insertion builds
//! its complete prospective row set before any of its rows becomes selectable, so
//! every inserted row's default observes the pre-statement state (both rows of one
//! bulk insert see the same `count(/items)`), while a later single-statement
//! insert observes the committed batch.

mod support;

use liasse_runtime::{CallRequest, Value};
use liasse_store::MemoryStore;
use liasse_value::{Integer, Text};
use support::{generator, load};

const BULK: &str = r#"{
  "$liasse": 1
  "$app": "t.bulk@1.0.0"
  "$model": {
    "staging": { "$key": "name", "name": "text" }
    "items": {
      "$key": "id"
      "id": "text"
      "name": "text"
      "pos": "int = count(/items) + 1"
    }
    "items_view": { "$view": ".items { name, pos }" }
    "$mut": {
      "bulk": ".items + .staging { id: .name, name: .name }"
      "add_one": ".items + { id: @id, name: @name }"
    }
  }
  "$data": { "staging": { "x": {}, "y": {} } }
}"#;

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

fn pos_by_name(engine: &liasse_runtime::Engine<MemoryStore>) -> Vec<(String, Value)> {
    let view = engine.view_at_head("items_view").expect("view").expect("declared");
    view.rows()
        .iter()
        .map(|row| {
            let name = match row.field("name") {
                Some(Value::Text(t)) => t.as_str().to_owned(),
                other => panic!("name: {other:?}"),
            };
            (name, row.field("pos").cloned().expect("pos"))
        })
        .collect()
}

#[test]
fn a_bulk_insert_resolves_defaults_against_prestatement_state() {
    let mut engine = load("bulk", BULK);
    let mut generator = generator();
    engine
        .call(&CallRequest::new("bulk"), &mut generator)
        .expect("call")
        .committed_at()
        .expect("bulk commits");
    // Both rows of one bulk insert compute count(/items) = 0, so pos = 1 for both;
    // neither observes its sibling (§5.1).
    let after_bulk = pos_by_name(&engine);
    assert_eq!(after_bulk, vec![("x".to_owned(), int(1)), ("y".to_owned(), int(1))]);

    // A separate later statement observes both committed rows: pos = 3.
    engine
        .call(&CallRequest::new("add_one").arg("id", Value::Text(Text::new("z"))).arg("name", Value::Text(Text::new("z"))), &mut generator)
        .expect("call")
        .committed_at()
        .expect("add_one commits");
    let after_one = pos_by_name(&engine);
    assert_eq!(
        after_one,
        vec![("x".to_owned(), int(1)), ("y".to_owned(), int(1)), ("z".to_owned(), int(3))],
    );
}
