#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.7 whole-collection replacement (`.coll = view`), driven through the real
//! engine. Each expectation is re-derived from §8.7 and §21.1: replacement
//! matches existing rows by key (a matching key updates, a new key inserts, a
//! dropped key is deleted through ordinary `$on_delete` planning), the engine
//! validates the complete resulting collection before admission, and a dropped
//! `restrict`-ref target rejects the whole transition atomically.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, RejectionReason};
use liasse_store::MemoryStore;
use liasse_value::Value;

use support::{generator, load};

/// A package whose `replace_users` mutation replaces the whole `users`
/// collection from the `imports` view (§8.7). A `tasks.owner` ref into `users`
/// carries `restrict`, so a replacement dropping a referenced user must be
/// rejected (§21.1). `imports` and `tasks` seeds are supplied per case.
fn replace_pkg(imports: &str, tasks: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1,
  "$app": "example.replace@1.0.0",
  "$model": {{
    "users": {{
      "$key": "id",
      "$unique": ["email"],
      "id": "text",
      "email": "text"
    }},
    "imports": {{ "$key": "id", "id": "text", "email": "text" }},
    "tasks": {{
      "$key": "id",
      "id": "text",
      "owner": {{ "$ref": "/users", "$on_delete": "restrict" }}
    }},
    "all_users": {{ "$view": ".users {{ id, email }}" }},
    "$mut": {{ "replace_users": ".users = .imports {{ id, email }}" }}
  }},
  "$data": {{
    "users": {{ "a": {{ "email": "a@x" }}, "b": {{ "email": "b@x" }} }},
    "imports": {imports},
    "tasks": {tasks}
  }}
}}"#
    )
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut generator = generator();
    engine.call(request, &mut generator).expect("call")
}

/// The `(id, email)` pairs of `all_users`, in the view's row order (B.5).
fn users(engine: &Engine<MemoryStore>) -> Vec<(String, String)> {
    let view = engine.view_at_head("all_users").expect("view").expect("declared");
    view.rows()
        .iter()
        .map(|row| (field(row.field("id")), field(row.field("email"))))
        .collect()
}

fn field(value: Option<&Value>) -> String {
    match value {
        Some(Value::Text(text)) => text.as_str().to_owned(),
        other => panic!("expected a text field, got {other:?}"),
    }
}

/// §8.7: one replacement statement inserts a new key, updates a matching key, and
/// deletes an absent key — atomically, in one commit. Here `a` is updated,
/// `c` is inserted, and `b` (unreferenced) is dropped.
#[test]
fn replacement_inserts_updates_deletes_atomically() {
    // `imports` holds `a` (new email) and `c` (new key); `b` is absent, so it is
    // dropped. `t1` references the surviving `a`, so the restrict ref is satisfied.
    let pkg = replace_pkg(
        r#"{ "a": { "email": "a2@x" }, "c": { "email": "c@x" } }"#,
        r#"{ "t1": { "owner": "a" } }"#,
    );
    let mut engine = load("replace-iud", &pkg);
    assert_eq!(users(&engine), vec![
        ("a".to_owned(), "a@x".to_owned()),
        ("b".to_owned(), "b@x".to_owned()),
    ]);

    let outcome = call(&mut engine, &CallRequest::new("replace_users"));
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "replacement commits, got {outcome:?}");

    // `a` kept its identity with the new email, `c` was inserted, `b` was deleted.
    assert_eq!(users(&engine), vec![
        ("a".to_owned(), "a2@x".to_owned()),
        ("c".to_owned(), "c@x".to_owned()),
    ]);
}

/// §8.7/§21.1: a replacement that drops a row a `restrict` ref still points at is
/// rejected as a whole — no partial effect, committed state intact.
#[test]
fn replacement_dropping_restrict_ref_target_rejects() {
    // `imports` holds only `a`, so `b` is dropped — but `t1` references `b` under
    // restrict, so the planned deletion (and the whole replacement) is rejected.
    let pkg = replace_pkg(
        r#"{ "a": { "email": "a@x" } }"#,
        r#"{ "t1": { "owner": "b" } }"#,
    );
    let mut engine = load("replace-restrict", &pkg);

    let outcome = call(&mut engine, &CallRequest::new("replace_users"));
    match outcome {
        CallOutcome::Rejected(rejection) => {
            assert_eq!(rejection.reason(), RejectionReason::Restricted, "{rejection:?}");
        }
        other => panic!("expected a restrict rejection, got {other:?}"),
    }

    // Nothing changed: the transition was rejected as a whole.
    assert_eq!(users(&engine), vec![
        ("a".to_owned(), "a@x".to_owned()),
        ("b".to_owned(), "b@x".to_owned()),
    ]);
}

/// §8.7: the engine validates the complete resulting collection before admission,
/// so a replacement that swaps two rows' unique emails admits (it collides only
/// pairwise, never in the final state).
#[test]
fn replacement_swapping_unique_values_admits() {
    // `imports` swaps the two users' emails; no `tasks` reference is involved.
    let pkg = replace_pkg(
        r#"{ "a": { "email": "b@x" }, "b": { "email": "a@x" } }"#,
        r#"{}"#,
    );
    let mut engine = load("replace-swap", &pkg);

    let outcome = call(&mut engine, &CallRequest::new("replace_users"));
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the swap admits, got {outcome:?}");

    assert_eq!(users(&engine), vec![
        ("a".to_owned(), "b@x".to_owned()),
        ("b".to_owned(), "a@x".to_owned()),
    ]);
}
