#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probe (§11.1/§11.3, §5.4): the engine.rs:1299 `$actor`/`$session`
//! composite-key binding fix. Its own doc states the actor row's key "is its
//! application-visible identity — a positional `Value::Composite` when the
//! actor/session collection is composite-keyed" and routes it through
//! `key_value_of` so the binding "addresses the stored N-component row". This
//! probe hands the engine exactly that `Value::Composite` actor key over a
//! composite-keyed `accounts` collection and asserts the admitted mutation
//! resolves `$actor` to the stored composite row. Every expectation is
//! re-derived from §5.4 (a composite key identity is the ordered tuple of its
//! components) and §11.1/§11.3 ($actor is the resolved application row, re-
//! materialized from committed state at admission).

mod support;

use liasse_runtime::{CallOutcome, CallRequest, ResponseValue, Value};
use liasse_value::Text;
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// The composite actor identity `[org, user]` in `$key` order (§5.4).
fn composite(org: &str, user: &str) -> Value {
    Value::Composite(vec![text(org), text(user)])
}

fn response_field(response: &Option<ResponseValue>, field: &str) -> String {
    let wire = response.as_ref().expect("a return value").to_wire();
    wire.get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("field `{field}` missing in {wire}"))
        .to_owned()
}

/// A composite-keyed `accounts` collection (`$key: [org, user]`) with a `$auth`
/// authenticator resolving `$actor` from it, and a mutation that reads the actor
/// row's own fields back. Seeded with one composite account so admission can
/// re-materialize it by its composite key.
const COMPOSITE_AUTH: &str = r#"{
  "$liasse": 1,
  "$app": "t.compauth@1.0.0",
  "$model": {
    "accounts": {
      "$key": ["org", "user"],
      "org": "text",
      "user": "text",
      "name": "text"
    },
    "$mut": {
      "who_am_i": "return $actor { org, user, name }"
    },
    "$auth": {
      "api": {
        "$credential": "text",
        "$verify": "$credential",
        "$actor": "/accounts[{ org: $proof.org, user: $proof.user }]"
      }
    }
  },
  "$data": {
    "accounts": { "acme:alice": { "org": "acme", "user": "alice", "name": "Alice" } }
  }
}"#;

#[test]
fn composite_actor_row_is_materialized_from_the_composite_key() {
    let mut engine = load("compauth", COMPOSITE_AUTH);
    let mut generator = generator();

    // §11.1/§11.3: the admission carries the resolved `$actor` composite key.
    // §5.4: that identity is the positional tuple `[acme, alice]`. The engine must
    // re-materialize the stored composite row, so `$actor.name` reads "Alice".
    let request = CallRequest::new("who_am_i").actor(composite("acme", "alice"));
    let outcome = engine.call(&request, &mut generator).expect("call");
    let response = match outcome {
        CallOutcome::Unchanged { response } | CallOutcome::Committed { response, .. } => response,
        other => panic!("who_am_i over a composite actor should yield a value, got {other:?}"),
    };
    assert_eq!(response_field(&response, "org"), "acme");
    assert_eq!(response_field(&response, "user"), "alice");
    assert_eq!(response_field(&response, "name"), "Alice", "$actor.name reads the composite row");
}
