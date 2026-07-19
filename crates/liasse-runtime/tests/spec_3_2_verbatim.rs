#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team reproduction (§3.2/§3.3): the verbatim "complete small application"
//! tasks package must not silently no-op through its local-binding `add_task`.
//!
//! The §3.2 root mutation is the two-statement local-binding form
//! `task = .tasks + { title: @title }` then `return task { … }`. §3.3 pins that
//! calling it COMMITS a new row and returns the created row `{ id, title, done,
//! created_at }` with the title normalized by `$normalize: string.trim`. A prior
//! runtime dropped the local-binding insert: the call reported success
//! `Unchanged` with no row inserted and no response — a success-reported
//! data-loss. This test drives the §3.2 package verbatim and asserts the commit,
//! the returned §3.3 shape, and that the committed view then holds the row.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Value};
use liasse_value::Text;
use support::{generator, load};

/// The §3.2 package, verbatim: the `add_task` root mutation is the local-binding
/// insert-and-return form the specification lists, the `tasks` row carries the
/// `$normalize`/`$check` title, generated `id`, `done` default, and `created_at`,
/// and `open_tasks` is the §3.2 live view. An `all_tasks` inspection view is
/// added (reading `done`) so the test can read the committed `done` default.
const TASKS: &str = r#"{
  "$liasse": 1
  "$app": "example.tasks@1.0.0"
  "$model": {
    "tasks": {
      "$key": "id"
      "id": "uuid = uuid()"
      "title": {
        "$type": "text"
        "$normalize": "string.trim(.)"
        "$check": ["size(.) > 0", "A title is required"]
      }
      "done": "bool = false"
      "created_at": "timestamp = now()"
      "$mut": {
        "complete": [
          ".done = true"
          "return . { id, title, done, created_at }"
        ]
      }
    }
    "open_tasks": {
      "$view": ".tasks[:task | !task.done] { id, title, created_at, $sort: [-created_at] }"
    }
    "all_tasks": { "$view": ".tasks { id, title, done, created_at }" }
    "$mut": {
      "add_task": [
        "task = .tasks + { title: @title }"
        "return task { id, title, done, created_at }"
      ]
    }
  }
}"#;

fn add_task(engine: &mut liasse_runtime::Engine<liasse_store::MemoryStore>, title: &str) -> CallOutcome {
    let mut generator = generator();
    engine
        .call(&CallRequest::new("add_task").arg("title", Value::Text(Text::new(title))), &mut generator)
        .expect("the call reaches admission")
}

#[test]
fn verbatim_add_task_commits_the_row_and_returns_the_spec_3_3_shape() {
    let mut engine = load("tasks-3-2", TASKS);
    let head = engine.head().unwrap();

    // §3.3: `add` with a whitespace-padded title commits a new row (not a
    // success-reported no-op) and advances the frontier.
    let outcome = add_task(&mut engine, "  Read the specification  ");
    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "the verbatim local-binding insert must COMMIT, not silently no-op: {outcome:?}"
    );
    assert_ne!(engine.head().unwrap(), head, "a committed insert advances the frontier");

    // §3.3: the call returns the created row, title normalized by `string.trim`.
    let response = outcome.response().expect("§3.3: `add` returns the created row").to_wire();
    let obj = response.as_object().expect("§3.3 result is the created row's fields");
    assert_eq!(obj.get("title").and_then(|v| v.as_str()), Some("Read the specification"), "§3.3 title trimmed");
    assert_eq!(obj.get("done"), Some(&serde_json::Value::Bool(false)), "§3.3 done: false");
    assert!(obj.contains_key("id"), "§3.3 result carries the generated id");
    assert!(obj.contains_key("created_at"), "§3.3 result carries created_at");

    // §3.3 steps 5-6: the committed live view then holds the new row.
    let open = engine.view_at_head("open_tasks").expect("view").expect("declared");
    assert_eq!(open.len(), 1, "§3.3: after `add` commits, open_tasks holds the new row");
    let all = engine.view_at_head("all_tasks").expect("view").expect("declared");
    let committed_id = all.rows()[0].field("id").expect("committed id").to_wire();
    assert_eq!(obj.get("id"), Some(&committed_id), "§8.10: the returned id is the committed row's id");
}
