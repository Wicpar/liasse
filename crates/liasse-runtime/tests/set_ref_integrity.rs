#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §5.5 set of `$ref` reference integrity (§5.6/§22.1): every member of a
//! `$set` of `$ref` is a reference that MUST resolve to a live row, exactly like
//! a scalar ref field — a dangling member rejects the whole transition. An atomic
//! rekey of a referenced row rewrites every inbound set member in the same
//! transition (§5.4). Every expectation is re-derived from the cited spec text.

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

fn commit(outcome: CallOutcome) {
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "expected a commit, got {outcome:?}");
}

const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.setref@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text", "name": "text" },
    "docs": { "$key": "id", "id": "text", "reviewers": { "$set": { "$ref": "/accounts" } } },
    "docs_view": { "$view": ".docs { id, reviewers }" },
    "$mut": {
      "add_account": ".accounts + { id: @id, name: @name }",
      "add_doc": ".docs + { id: @id }",
      "add_reviewer": ".docs[@id].reviewers + @acct",
      "rekey_account": ".accounts[@old].id = @new"
    }
  }
}"#;

fn reviewers(engine: &Engine<MemoryStore>) -> Vec<Value> {
    let view = engine.view_at_head("docs_view").expect("view").expect("declared");
    match view.rows()[0].field("reviewers") {
        Some(Value::Set(members)) => members.iter().cloned().collect(),
        other => panic!("reviewers is a set, got {other:?}"),
    }
}

#[test]
fn set_of_ref_dangling_member_is_rejected() {
    let mut engine = load("setref", M);
    let mut generator = generator();
    commit(engine.call(&CallRequest::new("add_doc").arg("id", text("d1")), &mut generator).expect("call"));
    let outcome = engine
        .call(
            &CallRequest::new("add_reviewer").arg("id", text("d1")).arg("acct", account_ref("ghost")),
            &mut generator,
        )
        .expect("call");
    assert_eq!(
        outcome.rejection().map(liasse_runtime::Rejection::reason),
        Some(RejectionReason::DanglingRef),
        "a set-of-ref member targeting a non-existent row must be rejected (§5.6/§22.1); got {outcome:?}"
    );
}

#[test]
fn set_of_ref_live_member_commits() {
    // The positive companion: a member targeting a live account commits and reads
    // back, so the rejection above is integrity enforcement, not a blanket refusal.
    let mut engine = load("setref", M);
    let mut generator = generator();
    commit(
        engine
            .call(&CallRequest::new("add_account").arg("id", text("a1")).arg("name", text("Alice")), &mut generator)
            .expect("call"),
    );
    commit(engine.call(&CallRequest::new("add_doc").arg("id", text("d1")), &mut generator).expect("call"));
    commit(
        engine
            .call(&CallRequest::new("add_reviewer").arg("id", text("d1")).arg("acct", account_ref("a1")), &mut generator)
            .expect("call"),
    );
    assert_eq!(reviewers(&engine), vec![account_ref("a1")], "a live set-of-ref member commits and reads back");
}

#[test]
fn rekey_rewrites_inbound_set_of_ref_member() {
    let mut engine = load("setref", M);
    let mut generator = generator();
    commit(
        engine
            .call(&CallRequest::new("add_account").arg("id", text("a1")).arg("name", text("Alice")), &mut generator)
            .expect("call"),
    );
    commit(engine.call(&CallRequest::new("add_doc").arg("id", text("d1")), &mut generator).expect("call"));
    commit(
        engine
            .call(&CallRequest::new("add_reviewer").arg("id", text("d1")).arg("acct", account_ref("a1")), &mut generator)
            .expect("call"),
    );
    commit(
        engine
            .call(&CallRequest::new("rekey_account").arg("old", text("a1")).arg("new", text("a2")), &mut generator)
            .expect("call"),
    );
    assert_eq!(
        reviewers(&engine),
        vec![account_ref("a2")],
        "§5.4: the inbound set-of-ref member must be rewritten from a1 to a2"
    );
}
