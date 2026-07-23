#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §22.1/§5.10/§5.5 red-team: a keyed-collection row `$check` MAY aggregate the
//! row's own nested child collection (the runtime finalize pass itself documents
//! "a check aggregating a nested child collection (`count(.departments) >= 0`)
//! resolves"). §22.1 makes that check a STATE CONSTRAINT that "holds in every
//! committed state" (it lists "field and row checks").
//!
//! Deleting a nested child row changes the parent's aggregate, so the parent's row
//! `$check` MUST be re-evaluated before the delete commits. Bug hunted: a nested
//! `collection - key` delete removes the child via a raw subtree wipe that never
//! marks the PARENT row touched, so the finalize pass (which re-validates only
//! touched rows) skips the parent check, committing a state that violates it.

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

fn committed(outcome: CallOutcome) {
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "expected a commit, got {outcome:?}");
}

/// A company whose row `$check` requires at least one office; offices are a nested
/// keyed collection under the company row. Seeded with one company holding one
/// office so the check is satisfied at genesis (a min-count check cannot be met by
/// an empty-collection insert, so the starting state comes from `$data`).
const PACKAGE: &str = r#"{
  "$liasse": 1,
  "$app": "example.nestedcheck@1.0.0",
  "$model": {
    "companies": {
      "$key": "cid",
      "cid": "text",
      "$check": ["count(.offices) >= 1", "a company must keep at least one office"],
      "offices": { "$key": "oid", "oid": "text" }
    },
    "companies_view": { "$view": ".companies { cid, $sort: [cid] }" },
    "$mut": {
      "add_office": ".companies[@cid].offices + { oid: @oid }",
      "delete_office": ".companies[@cid].offices - @oid"
    }
  },
  "$data": {
    "companies": {
      "acme": { "offices": { "paris": {} } }
    }
  }
}"#;

fn company_ids(engine: &Engine<MemoryStore>) -> Vec<Value> {
    engine
        .view_at_head("companies_view")
        .expect("view")
        .expect("declared")
        .rows()
        .iter()
        .map(|row| row.field("cid").expect("cid").clone())
        .collect()
}

#[test]
fn deleting_last_office_must_reject_on_parent_row_check() {
    let mut engine = load("nested-check-reject", PACKAGE);
    let head = engine.head().unwrap();

    // Deleting acme's only office leaves count(.offices) == 0, violating the company
    // row `$check`. §22.1: the check is a state constraint, so the transition MUST
    // be rejected — the pre-delete state remains.
    let outcome = call(
        &mut engine,
        &CallRequest::new("delete_office").arg("cid", text("acme")).arg("oid", text("paris")),
    );
    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "deleting the last office must be rejected by the parent row check (§22.1/§5.10), got {outcome:?}"
    );
    assert_eq!(engine.head().unwrap(), head, "a rejected delete leaves no commit");
    assert_eq!(company_ids(&engine), vec![text("acme")], "the company survives the rejected delete");
}

#[test]
fn deleting_one_of_two_offices_commits() {
    // Control: with two offices, deleting one leaves count(.offices) == 1, which
    // satisfies the check, so the delete commits.
    let mut engine = load("nested-check-ok", PACKAGE);
    committed(call(&mut engine, &CallRequest::new("add_office").arg("cid", text("acme")).arg("oid", text("lyon"))));

    let outcome = call(
        &mut engine,
        &CallRequest::new("delete_office").arg("cid", text("acme")).arg("oid", text("paris")),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "deleting one of two offices commits, got {outcome:?}");
}
