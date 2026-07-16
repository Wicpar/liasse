#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §21.1 `$on_delete` enforcement in the mutation delete path: a
//! `collection - key` delete is a graph operation whose inbound reference
//! policies (cascade, restrict, none/clear, `= patch`) decide the fate of the
//! rows that point at a deleted one. Each expectation is re-derived from §21.1.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, RejectionReason, Value};
use liasse_store::MemoryStore;
use liasse_value::{Ref, Text};
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn project_ref(id: &str) -> Value {
    Value::Ref(Ref::scalar(text(id)))
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut generator = generator();
    engine.call(request, &mut generator).expect("call")
}

fn add_project(engine: &mut Engine<MemoryStore>, id: &str, name: &str) {
    let outcome = call(engine, &CallRequest::new("add_project").arg("id", text(id)).arg("name", text(name)));
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "project {id} inserts");
}

fn add_task(engine: &mut Engine<MemoryStore>, id: &str, project: &str) {
    let outcome =
        call(engine, &CallRequest::new("add_task").arg("id", text(id)).arg("project", project_ref(project)));
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "task {id} inserts");
}

fn delete_project(engine: &mut Engine<MemoryStore>, id: &str) -> CallOutcome {
    call(engine, &CallRequest::new("delete_project").arg("id", text(id)))
}

fn task_ids(engine: &Engine<MemoryStore>) -> Vec<Value> {
    let view = engine.view_at_head("tasks_view").expect("view").expect("declared");
    view.rows().iter().map(|row| row.field("id").expect("id").clone()).collect()
}

/// projects/tasks with a required `project` ref under the given `$on_delete`
/// policy, a `status` field for patch tests, and a root delete mutation.
fn package(project_optional: bool, on_delete: &str) -> String {
    let optional = if project_optional { r#""$optional": true,"# } else { "" };
    format!(
        r#"{{
  "$liasse": 1,
  "$app": "example.ondelete@1.0.0",
  "$model": {{
    "projects": {{ "$key": "id", "id": "text", "name": "text = ''" }},
    "tasks": {{
      "$key": "id",
      "id": "text",
      "status": "text = 'active'",
      "archived_name": "text = ''",
      "project": {{ "$ref": "/projects", {optional} "$on_delete": "{on_delete}" }}
    }},
    "tasks_view": {{ "$view": ".tasks {{ id, status, archived_name, project, $sort: [id] }}" }},
    "projects_view": {{ "$view": ".projects {{ id }}" }},
    "$mut": {{
      "add_project": ".projects + {{ id: @id, name: @name }}",
      "add_task": ".tasks + {{ id: @id, project: @project }}",
      "delete_project": ".projects - @id"
    }}
  }}
}}"#
    )
}

#[test]
fn cascade_removes_referencing_rows_and_spares_others() {
    let mut engine = load("cascade", &package(false, "cascade"));
    add_project(&mut engine, "p1", "Apollo");
    add_project(&mut engine, "p2", "Gemini");
    add_task(&mut engine, "t1", "p1");
    add_task(&mut engine, "t2", "p1");
    add_task(&mut engine, "t3", "p2");

    assert!(matches!(delete_project(&mut engine, "p1"), CallOutcome::Committed { .. }));
    // §21.1: t1 and t2 cascade away with p1; t3 (referencing p2) survives.
    assert_eq!(task_ids(&engine), vec![text("t3")]);
}

#[test]
fn restrict_blocks_while_a_referencing_row_survives() {
    let mut engine = load("restrict", &package(false, "restrict"));
    add_project(&mut engine, "p1", "Apollo");
    add_task(&mut engine, "t1", "p1");
    let head = engine.head();

    let outcome = delete_project(&mut engine, "p1");
    assert_eq!(outcome.rejection().map(|r| r.reason()), Some(RejectionReason::Restricted));
    assert_eq!(engine.head(), head, "a blocked delete leaves no commit");
    assert_eq!(task_ids(&engine), vec![text("t1")], "state is intact");
}

#[test]
fn restrict_admits_once_the_reference_is_removed() {
    // The delete-task mutation is only present in this variant's package.
    let definition = package(false, "restrict").replace(
        r#""delete_project": ".projects - @id""#,
        "\"delete_project\": \".projects - @id\",\n      \"delete_task\": \".tasks - @id\"",
    );
    let mut engine = load("restrict2", &definition);
    add_project(&mut engine, "p1", "Apollo");
    add_task(&mut engine, "t1", "p1");

    assert!(matches!(delete_project(&mut engine, "p1"), CallOutcome::Rejected(_)));
    assert!(matches!(
        call(&mut engine, &CallRequest::new("delete_task").arg("id", text("t1"))),
        CallOutcome::Committed { .. }
    ));
    assert!(matches!(delete_project(&mut engine, "p1"), CallOutcome::Committed { .. }));
    assert!(task_ids(&engine).is_empty());
}

#[test]
fn patch_rewrites_the_surviving_row_and_clears_the_ref() {
    let mut engine = load("patch", &package(true, "= { project: none, status: 'orphaned' }"));
    add_project(&mut engine, "p1", "Apollo");
    add_task(&mut engine, "t1", "p1");

    assert!(matches!(delete_project(&mut engine, "p1"), CallOutcome::Committed { .. }));
    let view = engine.view_at_head("tasks_view").expect("view").expect("declared");
    let row = &view.rows()[0];
    assert_eq!(row.field("id"), Some(&text("t1")), "the task survives");
    assert_eq!(row.field("status"), Some(&text("orphaned")), "the patch rewrote status");
    // §21.1 clears the optional ref to `none`; a `none` optional field is an
    // absent optional value, so it is omitted from the projected view row.
    assert_eq!(row.field("project"), None, "the patch cleared the ref (absent)");
}

#[test]
fn patch_reads_a_field_off_the_deleted_target() {
    let mut engine =
        load("patch-target", &package(true, "= { project: none, archived_name: $target.name }"));
    add_project(&mut engine, "p1", "Apollo");
    add_task(&mut engine, "t1", "p1");

    assert!(matches!(delete_project(&mut engine, "p1"), CallOutcome::Committed { .. }));
    let view = engine.view_at_head("tasks_view").expect("view").expect("declared");
    let row = &view.rows()[0];
    // §21.1: the patch copied the vanishing project's name onto the survivor.
    assert_eq!(row.field("archived_name"), Some(&text("Apollo")));
    // The cleared optional ref is `none`, i.e. absent from the projection.
    assert_eq!(row.field("project"), None);
}
