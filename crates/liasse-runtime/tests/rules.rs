#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §5 dynamic rules at admission and §8 atomic admission, over the bank app:
//! each rule rejects at admission and leaves committed state intact.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, RejectionReason, Value};
use liasse_value::{Integer, Ref, Text};
use support::{generator, load, BANK};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

fn account_ref(id: &str) -> Value {
    Value::Ref(Ref::scalar(text(id)))
}

fn open(engine: &mut liasse_runtime::Engine<liasse_store::MemoryStore>, id: &str, email: &str) -> CallOutcome {
    let mut generator = generator();
    engine
        .call(&CallRequest::new("open_account").arg("id", text(id)).arg("email", text(email)), &mut generator)
        .expect("call")
}

fn reason(outcome: &CallOutcome) -> RejectionReason {
    outcome.rejection().expect("a rejection").reason()
}

#[test]
fn normalization_is_applied_at_admission() {
    let mut engine = load("bank", BANK);
    assert!(matches!(open(&mut engine, "a1", "  MiXeD@Case.COM  "), CallOutcome::Committed { .. }));
    let view = engine.view_at_head("all_accounts").expect("view").expect("declared");
    assert_eq!(view.rows()[0].field("email"), Some(&text("mixed@case.com")));
}

#[test]
fn duplicate_key_is_rejected() {
    let mut engine = load("bank", BANK);
    assert!(matches!(open(&mut engine, "a1", "one@x.com"), CallOutcome::Committed { .. }));
    let head = engine.head().unwrap();
    let outcome = open(&mut engine, "a1", "two@x.com");
    assert_eq!(reason(&outcome), RejectionReason::DuplicateKey);
    assert_eq!(engine.head().unwrap(), head, "a rejected duplicate leaves no commit");
}

#[test]
fn uniqueness_collision_is_rejected() {
    let mut engine = load("bank", BANK);
    assert!(matches!(open(&mut engine, "a1", "shared@x.com"), CallOutcome::Committed { .. }));
    // A different key but a colliding normalized email violates `$unique`.
    let outcome = open(&mut engine, "a2", "SHARED@X.com");
    assert_eq!(reason(&outcome), RejectionReason::Uniqueness);
}

#[test]
fn dangling_reference_is_rejected() {
    let mut engine = load("bank", BANK);
    let mut generator = generator();
    let outcome = engine
        .call(
            &CallRequest::new("add_membership").arg("id", text("m1")).arg("account", account_ref("ghost")),
            &mut generator,
        )
        .expect("call");
    assert_eq!(reason(&outcome), RejectionReason::DanglingRef);

    assert!(matches!(open(&mut engine, "a1", "a@x.com"), CallOutcome::Committed { .. }));
    let outcome = engine
        .call(
            &CallRequest::new("add_membership").arg("id", text("m1")).arg("account", account_ref("a1")),
            &mut generator,
        )
        .expect("call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "a resolvable reference admits");
}

#[test]
fn row_check_is_rejected() {
    let mut engine = load("bank", BANK);
    assert!(matches!(open(&mut engine, "a1", "a@x.com"), CallOutcome::Committed { .. }));
    let mut generator = generator();
    // Driving the balance negative fails the `.balance >= 0` row check.
    let outcome = engine
        .call(&CallRequest::new("set_balance").arg("id", text("a1")).arg("amount", int(-5)), &mut generator)
        .expect("call");
    assert_eq!(reason(&outcome), RejectionReason::Check);
    let view = engine.view_at_head("all_accounts").expect("view").expect("declared");
    assert_eq!(view.rows()[0].field("balance"), Some(&int(0)), "balance is unchanged");
}

#[test]
fn assertion_failure_is_rejected() {
    let mut engine = load("bank", BANK);
    assert!(matches!(open(&mut engine, "a1", "a@x.com"), CallOutcome::Committed { .. }));
    let mut generator = generator();
    let outcome = engine
        .call(&CallRequest::new("withdraw").receiver(text("a1")).arg("amount", int(50)), &mut generator)
        .expect("call");
    assert_eq!(reason(&outcome), RejectionReason::Assertion);
    assert_eq!(outcome.rejection().map(|r| r.message()), Some("Insufficient funds"));
}

#[test]
fn multi_statement_program_is_all_or_nothing() {
    let mut engine = load("bank", BANK);
    assert!(matches!(open(&mut engine, "a1", "a@x.com"), CallOutcome::Committed { .. }));
    let head = engine.head().unwrap();
    let mut generator = generator();
    // bump's third statement asserts a cap the first two statements exceed:
    // balance and email writes must leave no trace.
    let outcome = engine
        .call(
            &CallRequest::new("bump").receiver(text("a1")).arg("by", int(200)).arg("email", text("new@z.com")),
            &mut generator,
        )
        .expect("call");
    assert_eq!(reason(&outcome), RejectionReason::Assertion);
    assert_eq!(engine.head().unwrap(), head, "the failed program created no commit");
    let view = engine.view_at_head("all_accounts").expect("view").expect("declared");
    let row = &view.rows()[0];
    assert_eq!(row.field("balance"), Some(&int(0)), "balance untouched");
    assert_eq!(row.field("email"), Some(&text("a@x.com")), "email untouched");
}

#[test]
fn no_change_returns_unchanged_without_a_commit() {
    let mut engine = load("bank", BANK);
    assert!(matches!(open(&mut engine, "a1", "a@x.com"), CallOutcome::Committed { .. }));
    let head = engine.head().unwrap();
    let mut generator = generator();
    let outcome = engine
        .call(&CallRequest::new("deposit").receiver(text("a1")).arg("amount", int(0)), &mut generator)
        .expect("call");
    assert!(matches!(outcome, CallOutcome::Unchanged { .. }), "a zero deposit changes nothing");
    assert_eq!(engine.head().unwrap(), head, "unchanged advances no frontier");
}

#[test]
fn return_is_evaluated_from_committed_state() {
    let mut engine = load("bank", BANK);
    let mut generator = generator();
    engine
        .call(&CallRequest::new("add_person").arg("id", text("p1")).arg("name", text("Ada")), &mut generator)
        .expect("call");
    let outcome = engine
        .call(&CallRequest::new("rename").receiver(text("p1")).arg("name", text("Ada Lovelace")), &mut generator)
        .expect("call");
    let response = outcome.response().expect("a return value");
    assert_eq!(
        response.to_wire(),
        serde_json::json!({ "id": "p1", "name": "Ada Lovelace" }),
        "return reflects the committed rename"
    );
}
