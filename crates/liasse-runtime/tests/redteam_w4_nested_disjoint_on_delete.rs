#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM (WAVE 4) — §21.1: two struct-nested `$on_delete` effects on DISJOINT
//! leaves of ONE static struct OVER-REJECT as a spurious patch conflict.
//!
//! §21.1 (verbatim): "Patches to a surviving row combine when they touch disjoint
//! fields or assign the same resulting value; conflicting assignments reject the
//! transition."
//!
//! A `tasks` row carries a static struct `meta { owner1: $ref, owner2: $ref }`,
//! both optional with `$on_delete: "none"`. When one account is referenced by BOTH
//! `meta.owner1` AND `meta.owner2`, deleting it induces two `none`-clear effects on
//! the SAME struct but DISJOINT leaves. §21.1 says they combine — the delete
//! commits with both leaves cleared. The wave-3 struct-nested fix carried each
//! nested clear as a whole-`meta`-struct assignment, so both effects landed on the
//! top-level `meta` field with different values and collided
//! (`DeleteError::ConflictingPatch`), rejecting a spec-valid deletion.
//!
//! Expectations are derived from §21.1 alone. The CONTROL (a single nested clear)
//! already commits, isolating the defect to the disjoint-leaf combination.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::{Ref, Struct, Text};
use support::load;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn account_ref(id: &str) -> Value {
    Value::Ref(Ref::scalar(text(id)))
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut generator = support::generator();
    engine.call(request, &mut generator).expect("call")
}

/// accounts + tasks, where a task's `meta` struct holds two optional refs to an
/// account, both `$on_delete: "none"`.
const PACKAGE: &str = r#"{
  "$liasse": 1,
  "$app": "example.w4nested@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text" },
    "tasks": {
      "$key": "id",
      "id": "text",
      "meta": {
        "owner1": { "$ref": "/accounts", "$optional": true, "$on_delete": "none" },
        "owner2": { "$ref": "/accounts", "$optional": true, "$on_delete": "none" }
      }
    },
    "tasks_view": { "$view": ".tasks { id, meta, $sort: [id] }" },
    "$mut": {
      "add_account": ".accounts + { id: @id }",
      "add_task": ".tasks + { id: @id, meta: { owner1: @a, owner2: @b } }",
      "delete_account": ".accounts - @id"
    }
  }
}"#;

/// The `meta` struct of the single surviving task in `tasks_view`.
fn task_meta(engine: &Engine<MemoryStore>) -> Struct {
    let view = engine.view_at_head("tasks_view").expect("view").expect("declared");
    let row = &view.rows()[0];
    match row.field("meta") {
        Some(Value::Struct(meta)) => meta.clone(),
        // A struct with every optional member cleared may project as absent.
        None => Struct::new(Vec::new()),
        other => panic!("meta is a struct, got {other:?}"),
    }
}

// ── THE FINDING ──────────────────────────────────────────────────────────────
// One account referenced by BOTH `meta.owner1` and `meta.owner2`; deleting it
// induces two `none` clears on disjoint leaves of `meta`. §21.1: they combine, the
// delete commits, both leaves clear. The wave-3 fix rejected with ConflictingPatch.
#[test]
fn two_disjoint_nested_none_clears_combine_and_commit() {
    let mut engine = load("w4-nested-disjoint", PACKAGE);
    assert!(matches!(
        call(&mut engine, &CallRequest::new("add_account").arg("id", text("a1"))),
        CallOutcome::Committed { .. }
    ));
    assert!(matches!(
        call(
            &mut engine,
            &CallRequest::new("add_task")
                .arg("id", text("t1"))
                .arg("a", account_ref("a1"))
                .arg("b", account_ref("a1")),
        ),
        CallOutcome::Committed { .. }
    ));

    // §21.1: the two disjoint-leaf clears combine; the delete commits.
    let outcome = call(&mut engine, &CallRequest::new("delete_account").arg("id", text("a1")));
    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "two `$on_delete: none` effects on disjoint leaves of one struct combine (§21.1), got {outcome:?}",
    );

    // Both leaves cleared: `meta.owner1` and `meta.owner2` are now absent.
    let meta = task_meta(&engine);
    assert!(
        !matches!(meta.get("owner1"), Some(Value::Ref(_))),
        "owner1 cleared, got {:?}",
        meta.get("owner1"),
    );
    assert!(
        !matches!(meta.get("owner2"), Some(Value::Ref(_))),
        "owner2 cleared, got {:?}",
        meta.get("owner2"),
    );
}

// ── CONTROL: a single nested clear ────────────────────────────────────────────
// Deleting an account referenced by only ONE of the two leaves is a single nested
// effect, which already commits — the other leaf keeps its distinct, surviving ref.
#[test]
fn single_nested_clear_commits_and_spares_the_other_leaf() {
    let mut engine = load("w4-nested-single", PACKAGE);
    for id in ["a1", "a2"] {
        assert!(matches!(
            call(&mut engine, &CallRequest::new("add_account").arg("id", text(id))),
            CallOutcome::Committed { .. }
        ));
    }
    assert!(matches!(
        call(
            &mut engine,
            &CallRequest::new("add_task")
                .arg("id", text("t1"))
                .arg("a", account_ref("a1"))
                .arg("b", account_ref("a2")),
        ),
        CallOutcome::Committed { .. }
    ));

    // Deleting a1 clears only owner1; owner2 still points at the surviving a2.
    assert!(matches!(
        call(&mut engine, &CallRequest::new("delete_account").arg("id", text("a1"))),
        CallOutcome::Committed { .. }
    ));
    let meta = task_meta(&engine);
    assert!(!matches!(meta.get("owner1"), Some(Value::Ref(_))), "owner1 cleared");
    assert_eq!(meta.get("owner2"), Some(&account_ref("a2")), "owner2's surviving ref is intact");
}
