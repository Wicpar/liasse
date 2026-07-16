#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §11.1/§11.3 authenticated admission: a mutation program admitted through an
//! authenticated role binds `$actor` (and, when the authenticator declares one,
//! `$session`) to the resolved application row, so a program reading `$actor`
//! resolves it. A ref-typed field assigned `$actor` stores the actor's key (§5.6,
//! §6.3); a public call carrying no actor leaves `$actor` unbound and fails
//! closed. Every expectation is re-derived from §6/§11 text.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, ResponseValue, Value};
use liasse_value::Text;
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// The wire string at `field` of a single-row response object.
fn response_field(response: &Option<ResponseValue>, field: &str) -> String {
    let wire = response.as_ref().expect("a return value").to_wire();
    wire.get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("field `{field}` missing in {wire}"))
        .to_owned()
}

/// An app whose `$auth` resolves `$actor` from `/accounts` (and a `$session` from
/// `/sessions`), with a mutation writing `$actor` into a ref field and returning
/// the actor's own row fields. Seeded with one account so admission can
/// re-materialize it by key.
const AUTH: &str = r#"{
  "$liasse": 1
  "$app": "t.auth@1.0.0"
  "$model": {
    "accounts": {
      "$key": "id"
      "id": "text"
      "name": "text"
      "enabled": "bool = true"
    }
    "sessions": {
      "$key": "id"
      "id": "text"
      "account": { "$ref": "/accounts" }
      "expires_at": "timestamp"
      "revoked": "bool = false"
    }
    "notes": {
      "$key": "id"
      "id": "text"
      "author": { "$ref": "/accounts" }
      "body": "text"
    }
    "$mut": {
      "add_note({ id: text, body: text })": [
        "note = .notes + { id: @id, author: $actor, body: @body }"
        "return note { id, author, body }"
      ]
      "who_am_i": "return $actor { id, name }"
    }
    "$auth": {
      "session": {
        "$credential": "text"
        "$verify": "$credential"
        "$session": "/sessions[$proof.session]"
        "$actor": "/accounts[$session.account]"
      }
    }
  }
  "$data": {
    "accounts": { "alice": { "name": "Alice" } }
  }
}"#;


#[test]
fn authenticated_call_binds_actor_into_a_ref_field() {
    let mut engine = load("auth", AUTH);
    let mut generator = generator();

    // The call carries the resolved `$actor` account key; `author: $actor` stores
    // the actor's key (§5.6), which the `return` then projects.
    let request = CallRequest::new("add_note")
        .arg("id", text("n1"))
        .arg("body", text("hello"))
        .actor(text("alice"));
    let outcome = engine.call(&request, &mut generator).expect("call");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("authenticated add_note should commit, got {outcome:?}");
    };
    assert_eq!(response_field(&response, "author"), "alice", "author ref is the actor key");
    assert_eq!(response_field(&response, "body"), "hello");
}

#[test]
fn actor_row_field_access_reads_committed_state() {
    let mut engine = load("auth", AUTH);
    let mut generator = generator();

    // `$actor` is the whole account row, re-materialized from committed state, so
    // `$actor.name` reads the stored field.
    let request = CallRequest::new("who_am_i").actor(text("alice"));
    let outcome = engine.call(&request, &mut generator).expect("call");
    // No state change, so the query is delivered `unchanged` (§8.9).
    let response = match outcome {
        CallOutcome::Unchanged { response } | CallOutcome::Committed { response, .. } => response,
        other => panic!("who_am_i should yield a value, got {other:?}"),
    };
    assert_eq!(response_field(&response, "id"), "alice");
    assert_eq!(response_field(&response, "name"), "Alice", "$actor.name reads the row");
}

#[test]
fn public_call_without_actor_fails_closed() {
    let mut engine = load("auth", AUTH);
    let mut generator = generator();

    // Same mutation, no actor bound: `$actor` is in scope (the package declares an
    // authenticator) but unbound in this admission, so evaluating it faults and
    // the whole transition is rejected (§6.3 fail-closed), committing nothing.
    let request = CallRequest::new("add_note").arg("id", text("n2")).arg("body", text("nope"));
    let outcome = engine.call(&request, &mut generator).expect("call");
    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "a call with no actor must not admit a program reading $actor, got {outcome:?}"
    );
}
