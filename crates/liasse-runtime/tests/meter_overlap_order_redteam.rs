#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probe for §15.2/§15.3 pool draining order over overlapping bucketed
//! pools.
//!
//! §15.2 step 4-5: pools are sorted by `$order` then pool incarnation and drained
//! "in that order". With `$order: ["$until"]` and §14.3's ascending-optional
//! rule ("earliest finite end ... latest finite end, unbounded"), a spend must
//! drain the earliest-expiring pool first and the unbounded pool last (§15.3:
//! "The lifetime pool follows every finite expiry because ascending optional
//! order places `none` last"). We read the frozen `funding` (§15.6) to see the
//! exact per-pool allocation.

mod support;

use liasse_expr::Cell;
use liasse_runtime::{CallOutcome, CallRequest, Value};
use liasse_value::Decimal;

use support::{generator, store};

fn dec(text: &str) -> Value {
    Value::Decimal(Decimal::parse(text).expect("decimal"))
}

fn text(value: &str) -> Value {
    Value::Text(liasse_value::Text::new(value.to_owned()))
}

// Three overlapping bucketed pools: `early` (30, expires 2030), `mid` (100,
// expires 2040), `life` (1000, unbounded). Fixed clock (~2023-11) is before every
// expiry, so all three are active at the spend instant.
const OVERLAP: &str = r#"{
  "$liasse": 1,
  "$app": "t.meters.overlap@1.0.0",
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
        "occurred_at": "timestamp = now()"
      },
      "$limits": {
        "credits": {
          "$sources": { "topup": ".topups { $quantity: .amount }" },
          "$order": ["$until"]
        }
      },
      "$mut": {
        "consume": [ "spend = .spends + { amount: @amount }", "return spend { id, amount, funding }" ]
      }
    }
  },
  "$data": {
    "users": {
      "u1": {
        "topups": {
          "early": { "amount": "30", "expires_at": "1800000000" },
          "mid":   { "amount": "100", "expires_at": "1900000000" },
          "life":  { "amount": "1000" }
        }
      }
    }
  }
}"#;

#[test]
fn spend_drains_overlapping_pools_in_ascending_until_order() {
    let mut generator = generator();
    let mut engine = match liasse_runtime::Engine::load(store("meter-overlap"), OVERLAP, &mut generator) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error:?}"),
    };

    let outcome = engine
        .call(&CallRequest::new("consume").receiver(text("u1")).arg("amount", dec("150")), &mut generator)
        .expect("engine ok");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "spend of 150 fits total capacity: {outcome:?}");

    let response = outcome.response().expect("a response");
    let Cell::Row(row) = response.cell() else { panic!("response is not a row: {:?}", response.cell()) };
    let Some(Cell::Collection(funding)) = row.cell("funding") else {
        panic!("no funding collection on the spend response");
    };

    // Collect (pool-id, allocated-amount) from the frozen funding rows.
    let mut allocation: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for f in funding {
        let pool = match f.cell("pool") {
            Some(Cell::Scalar(Value::Text(t))) => t.as_str().to_owned(),
            other => panic!("funding pool not text: {other:?}"),
        };
        let amount = match f.cell("amount") {
            Some(Cell::Scalar(Value::Decimal(d))) => d.to_canonical_text(),
            other => panic!("funding amount not decimal: {other:?}"),
        };
        allocation.insert(pool, amount);
    }

    // §15.2/§15.3 + §14.3: ascending `$until`, none-last. A spend of 150 drains
    // early (30) fully, mid (100) fully, then 20 from the unbounded `life` pool.
    let early = allocation.get("early").map(String::as_str).unwrap_or("0");
    let mid = allocation.get("mid").map(String::as_str).unwrap_or("0");
    let life = allocation.get("life").map(String::as_str).unwrap_or("0");

    assert_eq!(
        (early, mid, life),
        ("30", "100", "20"),
        "ascending-$until drain must fund early=30, mid=100, life=20 (none-last); got {allocation:?}",
    );
}
