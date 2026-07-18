#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §5.6/§21.1/§22.1 `$on_delete` enforcement for a `$set` of `$ref` member: a set
//! member is a governed inbound reference, so its declared policy decides the
//! member's fate when the referenced row is deleted. `restrict` blocks the delete
//! while any member points at the target; `cascade` drops just that member from
//! the set (§5.6: "delete the containing row or set member"), leaving the
//! containing row alive and no dangling membership behind (§22.1). Every
//! expectation is re-derived from the cited spec text.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, RejectionReason, Value};
use liasse_store::MemoryStore;
use liasse_value::{Ref, Text};
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn account_ref(id: &str) -> Value {
    Value::Ref(Ref::scalar(text(id)))
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut generator = generator();
    engine.call(request, &mut generator).expect("call")
}

fn committed(outcome: CallOutcome) {
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "expected a commit, got {outcome:?}");
}

fn add_account(engine: &mut Engine<MemoryStore>, id: &str) {
    committed(call(engine, &CallRequest::new("add_account").arg("id", text(id)).arg("name", text(id))));
}

fn add_doc(engine: &mut Engine<MemoryStore>, id: &str) {
    committed(call(engine, &CallRequest::new("add_doc").arg("id", text(id))));
}

fn add_reviewer(engine: &mut Engine<MemoryStore>, doc: &str, account: &str) {
    committed(call(
        engine,
        &CallRequest::new("add_reviewer").arg("id", text(doc)).arg("acct", account_ref(account)),
    ));
}

fn delete_account(engine: &mut Engine<MemoryStore>, id: &str) -> CallOutcome {
    call(engine, &CallRequest::new("delete_account").arg("id", text(id)))
}

/// The `reviewers` set members of doc `d1`, in canonical read order.
fn reviewers(engine: &Engine<MemoryStore>) -> Vec<Value> {
    let view = engine.view_at_head("docs_view").expect("view").expect("declared");
    match view.rows()[0].field("reviewers") {
        Some(Value::Set(members)) => members.iter().cloned().collect(),
        // An emptied set is projected as absent.
        None => Vec::new(),
        other => panic!("reviewers is a set, got {other:?}"),
    }
}

fn account_ids(engine: &Engine<MemoryStore>) -> Vec<Value> {
    let view = engine.view_at_head("accounts_view").expect("view").expect("declared");
    view.rows().iter().map(|row| row.field("id").expect("id").clone()).collect()
}

fn doc_ids(engine: &Engine<MemoryStore>) -> Vec<Value> {
    let view = engine.view_at_head("docs_view").expect("view").expect("declared");
    view.rows().iter().map(|row| row.field("id").expect("id").clone()).collect()
}

/// docs whose `reviewers` set-of-ref members target `/accounts` under the given
/// `$on_delete` policy, plus a root delete mutation on accounts.
fn package(on_delete: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1,
  "$app": "example.setondelete@1.0.0",
  "$model": {{
    "accounts": {{ "$key": "id", "id": "text", "name": "text = ''" }},
    "docs": {{
      "$key": "id",
      "id": "text",
      "reviewers": {{ "$set": {{ "$ref": "/accounts", "$on_delete": "{on_delete}" }} }}
    }},
    "docs_view": {{ "$view": ".docs {{ id, reviewers, $sort: [id] }}" }},
    "accounts_view": {{ "$view": ".accounts {{ id, $sort: [id] }}" }},
    "$mut": {{
      "add_account": ".accounts + {{ id: @id, name: @name }}",
      "add_doc": ".docs + {{ id: @id }}",
      "add_reviewer": ".docs[@id].reviewers + @acct",
      "delete_account": ".accounts - @id"
    }}
  }}
}}"#
    )
}

#[test]
fn restrict_blocks_delete_of_referenced_account() {
    // §5.6: `restrict` preserves the target while any set member references it.
    let mut engine = load("set-restrict", &package("restrict"));
    add_account(&mut engine, "a1");
    add_doc(&mut engine, "d1");
    add_reviewer(&mut engine, "d1", "a1");
    let head = engine.head();

    let outcome = delete_account(&mut engine, "a1");
    assert_eq!(
        outcome.rejection().map(|r| r.reason()),
        Some(RejectionReason::Restricted),
        "a restrict set-of-ref member must block deletion of its target (§5.6/§21.1); got {outcome:?}"
    );
    assert_eq!(engine.head(), head, "a blocked delete leaves no commit");
    assert_eq!(reviewers(&engine), vec![account_ref("a1")], "state is intact");
    assert_eq!(account_ids(&engine), vec![text("a1")], "the referenced account survives");
}

#[test]
fn restrict_admits_delete_of_unreferenced_account() {
    // The positive companion: restrict blocks only while the ref exists, so an
    // account no set member points at is freely deletable.
    let mut engine = load("set-restrict-ok", &package("restrict"));
    add_account(&mut engine, "a1");
    add_account(&mut engine, "a2");
    add_doc(&mut engine, "d1");
    add_reviewer(&mut engine, "d1", "a1");

    committed(delete_account(&mut engine, "a2"));
    assert_eq!(account_ids(&engine), vec![text("a1")], "only the unreferenced account is gone");
    assert_eq!(reviewers(&engine), vec![account_ref("a1")], "the referencing member is untouched");
}

#[test]
fn cascade_drops_set_member_leaving_no_dangling() {
    // §5.6/§21.1: `cascade` on a set-of-ref member deletes the containing SET
    // MEMBER, not the whole row. Deleting a1 drops the a1 member from d1's set
    // while the doc and the still-live a2 member remain — §22.1 leaves no member
    // pointing at a removed row.
    let mut engine = load("set-cascade", &package("cascade"));
    add_account(&mut engine, "a1");
    add_account(&mut engine, "a2");
    add_doc(&mut engine, "d1");
    add_reviewer(&mut engine, "d1", "a1");
    add_reviewer(&mut engine, "d1", "a2");

    committed(delete_account(&mut engine, "a1"));
    assert_eq!(account_ids(&engine), vec![text("a2")], "a1 is deleted, a2 survives");
    assert_eq!(doc_ids(&engine), vec![text("d1")], "the containing doc row survives the cascade");
    assert_eq!(
        reviewers(&engine),
        vec![account_ref("a2")],
        "§22.1: the a1 member is dropped, no dangling membership remains"
    );
}

#[test]
fn cascade_emptying_the_set_leaves_the_row() {
    // Dropping the only member empties the set; the containing row still survives
    // (cascade removes the member, never the row, for a set-of-ref).
    let mut engine = load("set-cascade-empty", &package("cascade"));
    add_account(&mut engine, "a1");
    add_doc(&mut engine, "d1");
    add_reviewer(&mut engine, "d1", "a1");

    committed(delete_account(&mut engine, "a1"));
    assert_eq!(doc_ids(&engine), vec![text("d1")], "the doc survives with an empty reviewer set");
    assert!(reviewers(&engine).is_empty(), "§22.1: the sole dangling member is dropped");
}
