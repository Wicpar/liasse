//! §15 meter admission and accessor integration tests over a MemoryStore.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Value};
use liasse_value::Decimal;

use support::{generator, store};

fn dec(text: &str) -> Value {
    Value::Decimal(Decimal::parse(text).expect("decimal"))
}

fn text(value: &str) -> Value {
    Value::Text(liasse_value::Text::new(value.to_owned()))
}

const CREDITS: &str = r#"{
  "$liasse": 1,
  "$app": "t.meters.basic@1.0.0",
  "$semantics": { "timestamp_precision": "s" },
  "$model": {
    "users": {
      "$key": "id",
      "id": "text",
      "topups": { "$key": "id", "id": "text", "amount": "decimal" },
      "spends": {
        "$key": "id",
        "$consumes": "credits",
        "id": "uuid = uuid()",
        "amount": "decimal",
        "occurred_at": "timestamp = now()"
      },
      "$limits": { "credits": { "$sources": { "topup": ".topups { $quantity: .amount }" } } },
      "$mut": {
        "consume": [ "spend = .spends + { amount: @amount }", "return spend { id, amount }" ],
        "revoke": ".spends - @spend"
      }
    },
    "wallet": { "$view": ".users { id, balance: .credits.balance }" }
  },
  "$data": { "users": { "u1": { "topups": { "t1": { "amount": "100" } } } } }
}"#;

fn engine() -> liasse_runtime::Engine<liasse_store::MemoryStore> {
    let mut generator = generator();
    match liasse_runtime::Engine::load(store("meters"), CREDITS, &mut generator) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    }
}

fn balance(engine: &liasse_runtime::Engine<liasse_store::MemoryStore>) -> String {
    let view = engine.view_at_head("wallet").expect("view ok").expect("view present");
    let row = &view.rows()[0];
    match row.field("balance").expect("balance cell") {
        Value::Decimal(d) => d.to_canonical_text(),
        other => panic!("balance not a decimal: {other:?}"),
    }
}

#[test]
fn spend_within_capacity_admits_and_reduces_balance() {
    let mut engine = engine();
    let mut generator = generator();
    let outcome = engine
        .call(&CallRequest::new("consume").receiver(text("u1")).arg("amount", dec("40")), &mut generator)
        .expect("engine ok");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "spend within capacity admits: {outcome:?}");
    assert_eq!(balance(&engine), "60");
}

#[test]
fn spend_exceeding_capacity_rejects_and_preserves_balance() {
    let mut engine = engine();
    let mut generator = generator();
    let outcome = engine
        .call(&CallRequest::new("consume").receiver(text("u1")).arg("amount", dec("150")), &mut generator)
        .expect("engine ok");
    assert!(matches!(outcome, CallOutcome::Rejected(_)), "overspend rejects: {outcome:?}");
    assert_eq!(balance(&engine), "100", "a rejected spend drains nothing");
}

#[test]
fn deleting_a_spend_releases_its_allocation() {
    let mut engine = engine();
    let mut generator = generator();
    // Drain all capacity.
    let outcome = engine
        .call(&CallRequest::new("consume").receiver(text("u1")).arg("amount", dec("100")), &mut generator)
        .expect("engine ok");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "first spend should admit: {outcome:?}");
    let response = outcome.response().expect("a response");
    let spend_id = match response.cell() {
        liasse_expr::Cell::Row(row) => match row.cell("id") {
            Some(liasse_expr::Cell::Scalar(value)) => value.clone(),
            other => panic!("id cell: {other:?}"),
        },
        other => panic!("unexpected response {other:?}"),
    };
    assert_eq!(balance(&engine), "0");
    // A further spend now rejects.
    let outcome = engine
        .call(&CallRequest::new("consume").receiver(text("u1")).arg("amount", dec("10")), &mut generator)
        .expect("engine ok");
    assert!(matches!(outcome, CallOutcome::Rejected(_)));
    // Deleting the exhausting spend releases its allocation.
    let outcome = engine
        .call(&CallRequest::new("revoke").receiver(text("u1")).arg("spend", spend_id), &mut generator)
        .expect("engine ok");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "revoke admits: {outcome:?}");
    assert_eq!(balance(&engine), "100", "deletion releases the whole allocation");
}

const HIER: &str = r#"{
  "$liasse": 1,
  "$app": "t.meters.hier@1.0.0",
  "$model": {
    "companies": {
      "$key": "id", "id": "text",
      "topups": { "$key": "id", "id": "text", "amount": "decimal" },
      "$limits": { "credits": { "$sources": { "topup": ".topups { $quantity: .amount }" } } },
      "accounts": {
        "$key": "id", "id": "text",
        "topups": { "$key": "id", "id": "text", "amount": "decimal" },
        "$limits": { "credits": { "$sources": { "topup": ".topups { $quantity: .amount }" } } },
        "spends": { "$key": "id", "$consumes": "credits", "id": "uuid = uuid()", "amount": "decimal", "occurred_at": "timestamp = now()" },
        "$mut": { "consume": [ "spend = .spends + { amount: @amount }", "return spend { id }" ] }
      }
    }
  },
  "$data": { "companies": { "co": { "topups": { "ct": { "amount": "100" } }, "accounts": { "a1": { "topups": { "at1": { "amount": "50" } } } } } } }
}"#;

#[test]
fn hierarchical_clears_every_level() {
    let mut g = generator();
    let mut engine = match liasse_runtime::Engine::load(store("mhier"), HIER, &mut g) { Ok(e)=>e, Err(e)=>panic!("load: {e}") };
    let outcome = engine.call(&CallRequest::new("consume").receiver(text("co")).receiver(text("a1")).arg("amount", dec("40")), &mut g).expect("ok");
    assert!(matches!(outcome, CallOutcome::Committed{..}), "hierarchical admit: {outcome:?}");
}

#[test]
fn hierarchical_rejects_when_one_level_lacks_capacity() {
    let mut g = generator();
    let mut engine = match liasse_runtime::Engine::load(store("mhier2"), HIER, &mut g) { Ok(e) => e, Err(e) => panic!("load: {e}") };
    // account a1 holds only 50; the company holds 100. A spend of 60 clears the
    // company but not the account, so it rejects as one transition (§15.4).
    let outcome = engine
        .call(&CallRequest::new("consume").receiver(text("co")).receiver(text("a1")).arg("amount", dec("60")), &mut g)
        .expect("engine ok");
    assert!(matches!(outcome, CallOutcome::Rejected(_)), "account level lacks capacity: {outcome:?}");
    // Neither level was partially drained: a spend of 40 still fits both.
    let outcome = engine
        .call(&CallRequest::new("consume").receiver(text("co")).receiver(text("a1")).arg("amount", dec("40")), &mut g)
        .expect("engine ok");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "no partial drain left: {outcome:?}");
}

#[test]
fn exact_exhaustion_boundary_and_negative_amount() {
    let mut engine = engine();
    let mut g = generator();
    // A spend equal to the whole capacity admits; the next positive spend rejects.
    assert!(matches!(
        engine.call(&CallRequest::new("consume").receiver(text("u1")).arg("amount", dec("100")), &mut g).expect("ok"),
        CallOutcome::Committed { .. }
    ));
    assert_eq!(balance(&engine), "0");
    assert!(matches!(
        engine.call(&CallRequest::new("consume").receiver(text("u1")).arg("amount", dec("0.01")), &mut g).expect("ok"),
        CallOutcome::Rejected(_)
    ));
    // A zero spend needs no capacity and admits even against an exhausted meter.
    assert!(matches!(
        engine.call(&CallRequest::new("consume").receiver(text("u1")).arg("amount", dec("0")), &mut g).expect("ok"),
        CallOutcome::Committed { .. } | CallOutcome::Unchanged { .. }
    ));
    // A negative spend is rejected (§15.1) and never mints capacity.
    assert!(matches!(
        engine.call(&CallRequest::new("consume").receiver(text("u1")).arg("amount", dec("-5")), &mut g).expect("ok"),
        CallOutcome::Rejected(_)
    ));
    assert_eq!(balance(&engine), "0", "a rejected negative spend inflates nothing");
}
