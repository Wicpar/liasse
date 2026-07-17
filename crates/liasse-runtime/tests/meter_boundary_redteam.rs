#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probe for the half-open pool boundary at the exact `$until` instant.
//!
//! §14.1: a bucket interval is `[from, until)`; "At the upper bound, the row is no
//! longer active." §15.1: meter sources are evaluated in the temporal context of
//! the spend (`spend.$time`). So a spend whose `$time` equals a pool's `$until`
//! sees that pool as INACTIVE and must not draw capacity from it; one tick earlier
//! it is active.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Value};
use liasse_value::{Decimal, Integer, Timestamp, Precision};

use support::{generator, store};

fn dec(text: &str) -> Value {
    Value::Decimal(Decimal::parse(text).expect("decimal"))
}

fn text(value: &str) -> Value {
    Value::Text(liasse_value::Text::new(value.to_owned()))
}

fn ts(count: i128) -> Value {
    Value::Timestamp(Timestamp::new(count, Precision::Seconds))
}

// One bucketed pool `p` (amount 30, expires at second-count 1_800_000_000). The
// consume mutation takes an explicit `at` so the spend `$time` (= occurred_at) is
// controlled precisely.
const BOUNDARY: &str = r#"{
  "$liasse": 1,
  "$app": "t.meters.boundary@1.0.0",
  "$semantics": { "timestamp_precision": "s" },
  "$model": {
    "users": {
      "$key": "id",
      "id": "text",
      "topups": {
        "$key": "id",
        "$bucket": { "$until": ".expires_at" },
        "id": "text",
        "amount": "decimal",
        "expires_at": "timestamp? = none"
      },
      "spends": {
        "$key": "id",
        "$consumes": "credits",
        "id": "uuid = uuid()",
        "amount": "decimal",
        "occurred_at": "timestamp"
      },
      "$limits": {
        "credits": { "$sources": { "topup": ".topups { $quantity: .amount }" } }
      },
      "$mut": {
        "consume": [ "spend = .spends + { amount: @amount, occurred_at: @at }", "return spend { id }" ]
      }
    }
  },
  "$data": {
    "users": { "u1": { "topups": { "p": { "amount": "30", "expires_at": "1800000000" } } } }
  }
}"#;

#[test]
fn spend_at_exact_until_instant_sees_no_capacity() {
    let mut generator = generator();
    let mut engine = match liasse_runtime::Engine::load(store("meter-boundary"), BOUNDARY, &mut generator) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error:?}"),
    };

    // One tick BEFORE `$until`: the pool is active, so a spend of 10 admits.
    let before = engine
        .call(
            &CallRequest::new("consume")
                .receiver(text("u1"))
                .arg("amount", dec("10"))
                .arg("at", ts(1_799_999_999)),
            &mut generator,
        )
        .expect("engine ok");
    assert!(
        matches!(before, CallOutcome::Committed { .. }),
        "at until-1 the pool is active and funds the spend (§14.1): {before:?}",
    );

    // EXACTLY at `$until`: the half-open interval excludes this instant, so the
    // pool is inactive and a positive spend has no eligible capacity (§14.1/§15.1).
    let at = engine
        .call(
            &CallRequest::new("consume")
                .receiver(text("u1"))
                .arg("amount", dec("10"))
                .arg("at", ts(1_800_000_000)),
            &mut generator,
        )
        .expect("engine ok");
    assert!(
        matches!(at, CallOutcome::Rejected(_)),
        "at the exact $until instant the pool is inactive; a positive spend must \
         reject for lack of eligible capacity (§14.1 half-open, §15.1 spend-time \
         pool context): got {at:?}",
    );

    let _ = Integer::from(0);
}
