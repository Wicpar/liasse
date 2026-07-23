#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §22.1/§5.10 self-red-team companion to the nested-DELETE parent-check fix:
//! adding a nested child changes the parent's aggregate too, so a parent row
//! `$check` bounding the child collection from ABOVE (`count(.offices) <= 1`) must
//! also re-validate on a nested INSERT. Confirms the insert path marks the parent.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut generator = generator();
    engine.call(request, &mut generator).expect("call")
}

const PACKAGE: &str = r#"{
  "$liasse": 1,
  "$app": "example.nestedmax@1.0.0",
  "$model": {
    "companies": {
      "$key": "cid",
      "cid": "text",
      "$check": ["count(.offices) <= 1", "a company may hold at most one office"],
      "offices": { "$key": "oid", "oid": "text" }
    },
    "$mut": {
      "add_office": ".companies[@cid].offices + { oid: @oid }"
    }
  },
  "$data": {
    "companies": {
      "acme": { "offices": { "paris": {} } }
    }
  }
}"#;

#[test]
fn adding_a_second_office_must_reject_on_parent_max_check() {
    let mut engine = load("nested-max", PACKAGE);
    let head = engine.head().unwrap();

    // acme already holds one office; adding a second makes count(.offices) == 2,
    // violating count(.offices) <= 1. §22.1: the parent check must reject.
    let outcome =
        call(&mut engine, &CallRequest::new("add_office").arg("cid", text("acme")).arg("oid", text("lyon")));
    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "a nested insert breaching the parent max check must reject (§22.1/§5.10), got {outcome:?}"
    );
    assert_eq!(engine.head().unwrap(), head, "a rejected insert leaves no commit");
}
