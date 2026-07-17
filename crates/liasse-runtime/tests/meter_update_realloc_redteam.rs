#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probe for §15.2 spend-update reallocation conservation.
//!
//! §15.2: "Updating a spend provisionally releases its current allocation and
//! allocates its complete new amount, time, and metadata against the prospective
//! state." So reducing an exhausting spend must free its capacity before
//! reallocating the new (smaller) amount — never fund the new amount against the
//! capacity the same spend still holds.

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

const UPD: &str = r#"{
  "$liasse": 1,
  "$app": "t.meters.update@1.0.0",
  "$model": {
    "users": {
      "$key": "id",
      "id": "text",
      "topups": { "$key": "id", "id": "text", "amount": "decimal" },
      "spends": {
        "$key": "id",
        "$consumes": "credits",
        "id": "text",
        "amount": "decimal",
        "occurred_at": "timestamp = now()"
      },
      "$limits": { "credits": { "$sources": { "topup": ".topups { $quantity: .amount }" } } },
      "$mut": {
        "consume": [ "spend = .spends + { id: @id, amount: @amount }", "return spend { id }" ],
        "retarget": ".spends[@id].amount = @amount"
      }
    },
    "wallet": { "$view": ".users { id, balance: .credits.balance }" }
  },
  "$data": { "users": { "u1": { "topups": { "t1": { "amount": "100" } } } } }
}"#;

fn balance(engine: &liasse_runtime::Engine<liasse_store::MemoryStore>) -> String {
    let view = engine.view_at_head("wallet").expect("view ok").expect("view present");
    match view.rows()[0].field("balance").expect("balance cell") {
        Value::Decimal(d) => d.to_canonical_text(),
        other => panic!("balance not a decimal: {other:?}"),
    }
}

#[test]
fn reducing_an_exhausting_spend_releases_and_reallocates() {
    let mut generator = generator();
    let mut engine = match liasse_runtime::Engine::load(store("meter-update"), UPD, &mut generator) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error:?}"),
    };

    // Drain the whole 100-capacity pool with one spend.
    let outcome = engine
        .call(
            &CallRequest::new("consume").receiver(text("u1")).arg("id", text("s1")).arg("amount", dec("100")),
            &mut generator,
        )
        .expect("engine ok");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "initial spend admits: {outcome:?}");
    assert_eq!(balance(&engine), "0", "the pool is fully drained");

    // §15.2: updating the spend to 40 releases its 100 and reallocates 40.
    let outcome = engine
        .call(
            &CallRequest::new("retarget").receiver(text("u1")).arg("id", text("s1")).arg("amount", dec("40")),
            &mut generator,
        )
        .expect("engine ok");
    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "reducing the spend must admit — its own held capacity is released first \
         (§15.2), not counted against the new amount: {outcome:?}",
    );
    assert_eq!(
        balance(&engine),
        "60",
        "after releasing 100 and reallocating 40, remaining capacity is 60 (§15.2)",
    );
}
