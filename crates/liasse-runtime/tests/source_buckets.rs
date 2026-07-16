//! §14.4–§14.6 source-backed / recurring bucket materialization, and the §15
//! meter funding it unblocks: a spend draws from the source-derived pool active at
//! the spend instant, unspent capacity of a finished period does not roll over,
//! and a non-advancing or ill-bounded series rejects the transition that produces
//! it.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_runtime::{CallOutcome, CallRequest, FixedGenerators, Precision, Value};
use liasse_value::Decimal;
use support::store;

/// 2026-01-01T00:00:00Z in microseconds — the instant the weekly series starts, so
/// the first period is active at genesis.
const CLOCK_MICROS: i128 = 1_767_225_600_000_000;

fn generators() -> FixedGenerators {
    FixedGenerators::new(CLOCK_MICROS, Precision::Micros)
}

fn dec(text: &str) -> Value {
    Value::Decimal(Decimal::parse(text).expect("decimal"))
}

fn text(value: &str) -> Value {
    Value::Text(liasse_value::Text::new(value.to_owned()))
}

/// An account meter whose pool source is a recurring source-backed bucket
/// (`credit_periods`), one interval per weekly period per subscription.
const RECURRING: &str = r#"{
  "$liasse": 1, "$app": "t.b14.recurmeter@1.0.0",
  "$semantics": { "timestamp_precision": "s" },
  "$model": {
    "plans": { "$key": "id", "id": "text", "credits": "decimal", "period": "period?" },
    "subscriptions": {
      "$key": "id", "id": "text",
      "account": { "$ref": "/accounts" }, "plan": { "$ref": "/plans" },
      "starts_at": "timestamp", "ends_at": "timestamp? = none"
    },
    "credit_periods": {
      "$bucket": {
        "$source": ".subscriptions",
        "$from": "$source.starts_at",
        "$until": "$source.ends_at",
        "$repeat": "/plans[$source.plan].period"
      },
      "credits": "= /plans[$source.plan].credits"
    },
    "accounts": {
      "$key": "id", "id": "text",
      "$limits": { "credits": { "$sources": {
        "subscription": "/credit_periods[:p | p.$source.account == .] { $quantity: .credits }"
      }, "$order": ["$until"] } },
      "spends": {
        "$key": "id", "$consumes": "credits",
        "id": "uuid = uuid()", "amount": "decimal", "occurred_at": "timestamp = now()"
      },
      "$mut": { "consume": [ "spend = .spends + { amount: @amount }", "return spend { id }" ] }
    },
    "wal": { "$view": ".accounts { id, balance: .credits.balance }" }
  },
  "$data": {
    "plans": { "weekly": { "credits": "100", "period": "P7D" } },
    "accounts": { "a1": {} },
    "subscriptions": { "s1": { "account": "a1", "plan": "weekly", "starts_at": "1767225600" } }
  }
}"#;

fn balance(engine: &liasse_runtime::Engine<liasse_store::MemoryStore>) -> String {
    let view = engine.view_at_head("wal").expect("view ok").expect("view present");
    match view.rows()[0].field("balance").expect("balance cell") {
        Value::Decimal(d) => d.to_canonical_text(),
        other => panic!("balance not a decimal: {other:?}"),
    }
}

#[test]
fn recurring_pool_funds_spend_and_does_not_roll_over() {
    let mut g = generators();
    let mut engine = match liasse_runtime::Engine::load(store("recurmeter"), RECURRING, &mut g) {
        Ok(e) => e,
        Err(e) => panic!("load: {e}"),
    };
    // The first weekly period offers 100; a 40 spend leaves 60 in the active period.
    let outcome = engine
        .call(&CallRequest::new("consume").receiver(text("a1")).arg("amount", dec("40")), &mut g)
        .expect("engine ok");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "spend within capacity admits: {outcome:?}");
    assert_eq!(balance(&engine), "60");

    // Advance one week: the next period offers a fresh 100 and the finished
    // period's unspent 60 expires with it (§14.5 / §15.1 — pools are the rows
    // active at evaluation time; capacity does not roll over).
    engine.advance(604_800_000_000);
    assert_eq!(balance(&engine), "100");
}

#[test]
fn recurring_pool_rejects_overspend_of_active_period() {
    let mut g = generators();
    let mut engine =
        liasse_runtime::Engine::load(store("recurmeter2"), RECURRING, &mut g).expect("load");
    let outcome = engine
        .call(&CallRequest::new("consume").receiver(text("a1")).arg("amount", dec("150")), &mut g)
        .expect("engine ok");
    assert!(matches!(outcome, CallOutcome::Rejected(_)), "one period funds at most its 100: {outcome:?}");
    assert_eq!(balance(&engine), "100", "a rejected spend drains nothing");
}
