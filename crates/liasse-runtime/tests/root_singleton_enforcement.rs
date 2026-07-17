#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.2 root singleton state constraints (§22.1): the field/row checks and
//! reference validity that hold in every committed state apply to the reserved
//! root singleton row exactly as they do to a keyed collection row. A writable
//! field declared directly under `$model` is a root singleton field; its
//! `$check` must reject an invalid value (§8.8), its `$ref` must resolve to a
//! live row (§5.6), and an atomic rekey of a referenced row must rewrite the
//! singleton's inbound ref in the same transition (§5.4). Every expectation is
//! re-derived from the cited spec text; the keyed-collection control cases prove
//! the constraint is real, not merely mirrored.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, RejectionReason, Value};
use liasse_store::MemoryStore;
use liasse_value::{Integer, Ref, Text};
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

fn account_ref(id: &str) -> Value {
    Value::Ref(Ref::scalar(text(id)))
}

fn reason(outcome: &CallOutcome) -> Option<RejectionReason> {
    outcome.rejection().map(liasse_runtime::Rejection::reason)
}

fn commit(outcome: CallOutcome) {
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "expected a commit, got {outcome:?}");
}

const CHECK_MODEL: &str = r#"{
  "$liasse": 1,
  "$app": "t.rootchk@1.0.0",
  "$model": {
    "count": { "$type": "int", "$default": "= 0", "$check": ["(. >= 0)", "no negative count"] },
    "items": { "$key": "id", "id": "text", "n": { "$type": "int", "$check": ["(. >= 0)", "no negative n"] } },
    "$mut": { "set_count": ".count = @n", "add_item": ".items + { id: @id, n: @n }" }
  }
}"#;

// Control (PASSES with or without the fix): the same check on a keyed collection
// rejects -5, proving the check is externally deducible from `(. >= 0)`.
#[test]
fn collection_field_check_is_enforced_control() {
    let mut engine = load("rootchk", CHECK_MODEL);
    let mut g = generator();
    let outcome = engine
        .call(&CallRequest::new("add_item").arg("id", text("i1")).arg("n", int(-5)), &mut g)
        .expect("call");
    assert_eq!(reason(&outcome), Some(RejectionReason::Check), "control: {outcome:?}");
}

#[test]
fn root_singleton_field_check_is_enforced() {
    let mut engine = load("rootchk", CHECK_MODEL);
    let mut g = generator();
    let outcome = engine.call(&CallRequest::new("set_count").arg("n", int(-5)), &mut g).expect("call");
    assert_eq!(
        reason(&outcome),
        Some(RejectionReason::Check),
        "a root singleton field $check must reject an invalid value (§8.8/§22.1); got {outcome:?}"
    );
}

const REF_MODEL: &str = r#"{
  "$liasse": 1,
  "$app": "t.rootref@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text", "name": "text" },
    "owner": { "$ref": "/accounts", "$optional": true },
    "owner_view": { "$view": ".owner" },
    "$mut": {
      "add_account": ".accounts + { id: @id, name: @name }",
      "set_owner": ".owner = @acct",
      "rekey_account": ".accounts[@old].id = @new"
    }
  }
}"#;

fn owner(engine: &Engine<MemoryStore>) -> Option<Value> {
    engine.view_at_head("owner_view").expect("v").expect("d").scalar().cloned()
}

#[test]
fn root_singleton_ref_integrity_is_enforced() {
    let mut engine = load("rootref", REF_MODEL);
    let mut g = generator();
    let outcome =
        engine.call(&CallRequest::new("set_owner").arg("acct", account_ref("ghost")), &mut g).expect("call");
    assert_eq!(
        reason(&outcome),
        Some(RejectionReason::DanglingRef),
        "a root singleton ref to a non-existent row must be rejected (§5.6/§22.1); got {outcome:?}"
    );
}

#[test]
fn root_singleton_ref_to_live_row_commits() {
    // The positive companion: a singleton ref to a live account commits and reads
    // back, so the rejection above is integrity enforcement, not a blanket refusal.
    let mut engine = load("rootref", REF_MODEL);
    let mut g = generator();
    commit(
        engine
            .call(&CallRequest::new("add_account").arg("id", text("a1")).arg("name", text("A")), &mut g)
            .expect("call"),
    );
    commit(engine.call(&CallRequest::new("set_owner").arg("acct", account_ref("a1")), &mut g).expect("call"));
    assert_eq!(owner(&engine), Some(account_ref("a1")), "a valid singleton ref commits and reads back");
}

#[test]
fn rekey_rewrites_root_singleton_ref() {
    let mut engine = load("rootref", REF_MODEL);
    let mut g = generator();
    commit(
        engine
            .call(&CallRequest::new("add_account").arg("id", text("a1")).arg("name", text("A")), &mut g)
            .expect("call"),
    );
    commit(engine.call(&CallRequest::new("set_owner").arg("acct", account_ref("a1")), &mut g).expect("call"));
    commit(
        engine
            .call(&CallRequest::new("rekey_account").arg("old", text("a1")).arg("new", text("a2")), &mut g)
            .expect("call"),
    );
    assert_eq!(
        owner(&engine),
        Some(account_ref("a2")),
        "§5.4: the root singleton inbound ref must be rewritten from a1 to a2"
    );
}
