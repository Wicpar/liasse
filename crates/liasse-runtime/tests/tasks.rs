#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §3.2 tasks application end-to-end: add a task, observe it through a view,
//! complete it, and observe the view reflect the change — asserting the exact
//! normalized, defaulted, and generated field values (§5, §8).

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Timestamp, Value};
use liasse_value::{Precision, Text};
use support::{generator, load, NOW_MICROS, TASKS};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

#[test]
fn add_view_complete_cycle() {
    let mut engine = load("tasks", TASKS);
    let mut generator = generator();

    // add_task normalizes the title and fills id/done/created_at by default.
    let outcome = engine
        .call(&CallRequest::new("add_task").arg("title", text("  Buy milk  ")), &mut generator)
        .expect("call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "add_task should commit");

    // The open view shows exactly the one open task, with a trimmed title.
    let open = engine.view_at_head("open_tasks").expect("view").expect("declared");
    assert_eq!(open.len(), 1);
    let row = &open.rows()[0];
    assert_eq!(row.field("title"), Some(&text("Buy milk")), "title is trimmed");
    let id = row.field("id").cloned().expect("generated id");
    assert!(matches!(id, Value::Uuid(_)), "id is a generated uuid");

    // The inspection view proves the defaulted/generated fields.
    let all = engine.view_at_head("all_tasks").expect("view").expect("declared");
    let inspected = &all.rows()[0];
    assert_eq!(inspected.field("done"), Some(&Value::Bool(false)), "done defaults to false");
    assert_eq!(
        inspected.field("created_at"),
        Some(&Value::Timestamp(Timestamp::new(NOW_MICROS, Precision::Micros))),
        "created_at is the fixed now() sample"
    );

    // complete() is a row mutation selected by the task's key.
    let outcome = engine
        .call(&CallRequest::new("complete").receiver(id.clone()), &mut generator)
        .expect("call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "complete should commit");

    // The task leaves the open view; the inspection view shows it done.
    let open = engine.view_at_head("open_tasks").expect("view").expect("declared");
    assert!(open.is_empty(), "completed task leaves the open view");
    let all = engine.view_at_head("all_tasks").expect("view").expect("declared");
    assert_eq!(all.rows()[0].field("done"), Some(&Value::Bool(true)), "task is done");
}
