#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §5.2 computed values: a read-only computed member is derived from the row's
//! own state, participates in views, projections, and `return` like any other
//! value, reflects a later field change, and yields an absent optional when its
//! expression evaluates to `none`.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Value};
use liasse_value::Text;
use serde_json::json;
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// A people app whose `full` is a computed value over two writable fields, and
/// whose `contact` is a computed value mirroring an optional field.
const PEOPLE: &str = r#"{
  "$liasse": 1,
  "$app": "example.people@1.0.0",
  "$model": {
    "people": {
      "$key": "id",
      "id": "text",
      "first": "text",
      "last": "text",
      "email": "text?",
      "full": "= .first + ' ' + .last",
      "contact": "= .email"
    },
    "people_view": { "$view": ".people { id, full, contact }" },
    "$mut": {
      "add": [
        "row = .people + { id: @id, first: @first, last: @last }",
        "return row { id, full, contact }"
      ],
      "add_with_email": [
        "row = .people + { id: @id, first: @first, last: @last, email: @email }",
        "return row { id, full, contact }"
      ],
      "set_last({ id: text, last: text })": [
        ".people[@id].last = @last",
        "return .people[@id].full"
      ]
    }
  }
}"#;

#[test]
fn computed_value_derives_and_reflects_state() {
    let mut engine = load("people", PEOPLE);
    let mut generator = generator();

    // A computed value is projected in the mutation return from the admitted row.
    let outcome = engine
        .call(
            &CallRequest::new("add")
                .arg("id", text("p1"))
                .arg("first", text("Ada"))
                .arg("last", text("Byron")),
            &mut generator,
        )
        .expect("call");
    let response = outcome.response().expect("add returns a row").to_wire();
    // `contact` mirrors the omitted optional `email` -> none -> absent member.
    assert_eq!(response, json!({ "id": "p1", "full": "Ada Byron" }));

    // The same computed value is visible through a view.
    let view = engine.view_at_head("people_view").expect("view").expect("declared");
    assert_eq!(view.len(), 1);
    assert_eq!(view.rows()[0].field("full"), Some(&text("Ada Byron")));
    // An absent optional computed value does not appear as a view field.
    assert_eq!(view.rows()[0].field("contact"), None);

    // A later field change re-derives the computed value (read from committed
    // state through the row-selecting `return`).
    let outcome = engine
        .call(
            &CallRequest::new("set_last").arg("id", text("p1")).arg("last", text("Lovelace")),
            &mut generator,
        )
        .expect("call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }));
    let response = outcome.response().expect("set_last returns the computed value").to_wire();
    assert_eq!(response, json!("Ada Lovelace"));

    let view = engine.view_at_head("people_view").expect("view").expect("declared");
    assert_eq!(view.rows()[0].field("full"), Some(&text("Ada Lovelace")));
}

#[test]
fn present_optional_flows_into_computed_value() {
    let mut engine = load("people", PEOPLE);
    let mut generator = generator();

    let outcome = engine
        .call(
            &CallRequest::new("add_with_email")
                .arg("id", text("p2"))
                .arg("first", text("Grace"))
                .arg("last", text("Hopper"))
                .arg("email", text("grace@example.test")),
            &mut generator,
        )
        .expect("call");
    let response = outcome.response().expect("returns a row").to_wire();
    assert_eq!(
        response,
        json!({ "id": "p2", "full": "Grace Hopper", "contact": "grace@example.test" })
    );

    let view = engine.view_at_head("people_view").expect("view").expect("declared");
    assert_eq!(view.rows()[0].field("contact"), Some(&text("grace@example.test")));
}
