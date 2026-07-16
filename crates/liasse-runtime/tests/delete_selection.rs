#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8 prefix-minus delete of a selected row set (`-.coll[:x | pred]`) wired into
//! the §21.1 cascade planner, and §7.1 view-through-view resolution (a public
//! surface `$view` that names another declared view). Expectations are
//! re-derived from §8, §21.1, and §7.1.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

const P: &str = r#"{
  "$liasse": 1,
  "$app": "example.delsel@1.0.0",
  "$model": {
    "things": { "$key": "id", "id": "text" },
    "keep_ab": { "$view": ".things_view" },
    "things_view": { "$view": ".things { id, $sort: [id] }" },
    "$mut": {
      "seed3": [".things + { id: 'a' }", ".things + { id: 'b' }", ".things + { id: 'c' }"],
      "purge": "-.things[:x | x.id == @a || x.id == @b]"
    }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut generator = generator();
    engine.call(request, &mut generator).expect("call")
}

fn ids(engine: &Engine<MemoryStore>, view: &str) -> Vec<Value> {
    let result = engine.view_at_head(view).expect("view").expect("declared");
    result.rows().iter().map(|r| r.field("id").expect("id").clone()).collect()
}

#[test]
fn prefix_minus_deletes_the_selected_rows() {
    let mut engine = load("delsel", P);
    assert!(matches!(call(&mut engine, &CallRequest::new("seed3")), CallOutcome::Committed { .. }));
    assert_eq!(ids(&engine, "things_view"), vec![text("a"), text("b"), text("c")]);

    // §8: `-.things[:x | x.id == @a || x.id == @b]` removes exactly a and b.
    assert!(matches!(
        call(&mut engine, &CallRequest::new("purge").arg("a", text("a")).arg("b", text("b"))),
        CallOutcome::Committed { .. }
    ));
    assert_eq!(ids(&engine, "things_view"), vec![text("c")]);
}

#[test]
fn view_resolves_through_another_named_view() {
    let mut engine = load("delsel2", P);
    assert!(matches!(call(&mut engine, &CallRequest::new("seed3")), CallOutcome::Committed { .. }));
    // §7.1: `keep_ab` is `.things_view`, a bare reference to another declared
    // view — it must resolve to the same row set the referenced view produces.
    assert_eq!(ids(&engine, "keep_ab"), vec![text("a"), text("b"), text("c")]);
}
