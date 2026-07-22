#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.10 with §10.3: a mutation `return $actor { … }` is evaluated from the
//! FINAL admitted state, so when the program itself writes the actor's own row —
//! the actor disabling (or re-enabling) its `$members` row, or changing a field a
//! computed value derives from — the returned `$actor` reflects the resulting
//! committed state, NOT the admission-position snapshot bound before the program
//! ran. This is the value-level twin of the corpus case
//! `10-interfaces-roles/membership-reevaluated-each-admission`, whose step-2 `quit`
//! return asserted `enabled: false`. Every expectation is re-derived from
//! §8.10 ("The response is evaluated from the final admitted state: the committed
//! resulting state") and the §5.2 computed-value rule; none is read back from the
//! implementation.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, ResponseValue, Value};
use liasse_value::Text;
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// The whole single-row response object as JSON.
fn row(response: &Option<ResponseValue>) -> serde_json::Value {
    response.as_ref().expect("a return value").to_wire()
}

/// The committed/unchanged response of a call that must not fault.
fn served(outcome: CallOutcome) -> Option<ResponseValue> {
    match outcome {
        CallOutcome::Committed { response, .. } | CallOutcome::Unchanged { response } => response,
        other => panic!("call should have served a value, got {other:?}"),
    }
}

/// One account collection whose row carries a stored `enabled` flag (a `$members`
/// predicate reads it), two name fields, and a computed `full` derived from them.
/// Root mutations write the actor's own row through `/accounts[$actor.$key]`, then
/// return `$actor` — the exact shape the role-scoped corpus case exercises.
const READM: &str = r#"{
  "$liasse": 1
  "$app": "t.readm_actor@1.0.0"
  "$model": {
    "accounts": {
      "$key": "id"
      "id": "text"
      "enabled": "bool = true"
      "first": "text"
      "last": "text"
      "full": "= .first + ' ' + .last"
    }
    "$mut": {
      "quit": [
        "/accounts[$actor.$key].enabled = false"
        "return $actor { id, enabled }"
      ]
      "rejoin": [
        "/accounts[$actor.$key].enabled = true"
        "return $actor { id, enabled }"
      ]
      "flip_thrice": [
        "/accounts[$actor.$key].enabled = false"
        "/accounts[$actor.$key].enabled = true"
        "/accounts[$actor.$key].enabled = false"
        "return $actor { id, enabled }"
      ]
      "rename({ last: text })": [
        "/accounts[$actor.$key].last = @last"
        "return $actor { id, full }"
      ]
      "disable_other({ id: text })": [
        "/accounts[@id].enabled = false"
        "return $actor { id, enabled }"
      ]
    }
    "$auth": {
      "token": {
        "$credential": "text"
        "$verify": "$credential"
        "$actor": "/accounts[$proof]"
      }
    }
  }
  "$data": {
    "accounts": {
      "alice": { "first": "Ada", "last": "Byron" }
      "bob": { "enabled": false, "first": "Grace", "last": "Hopper" }
    }
  }
}"#;

/// enabled → disabled: the actor disables its own row, and the return observes the
/// resulting `enabled: false` (§8.10), not the admission snapshot's `true`.
#[test]
fn actor_return_observes_self_disable() {
    let mut engine = load("readm", READM);
    let request = CallRequest::new("quit").actor(text("alice"));
    let response = served(engine.call(&request, &mut generator()).expect("call"));
    let row = row(&response);
    assert_eq!(row.get("id").and_then(serde_json::Value::as_str), Some("alice"));
    assert_eq!(
        row.get("enabled").and_then(serde_json::Value::as_bool),
        Some(false),
        "$actor.enabled in the return must reflect the resulting state, not the pre-transition snapshot"
    );
}

/// disabled → enabled: the reverse flip. bob starts disabled; the return observes
/// the resulting `enabled: true`.
#[test]
fn actor_return_observes_self_enable() {
    let mut engine = load("readm", READM);
    let request = CallRequest::new("rejoin").actor(text("bob"));
    let response = served(engine.call(&request, &mut generator()).expect("call"));
    assert_eq!(
        row(&response).get("enabled").and_then(serde_json::Value::as_bool),
        Some(true),
        "$actor.enabled must reflect the resulting re-enable"
    );
}

/// Several writes to the actor's own field in one program: the return reads the
/// FINAL prospective (§8.10), so false→true→false yields `false`, never an
/// intermediate or the admission snapshot.
#[test]
fn actor_return_reads_final_of_multiple_flips() {
    let mut engine = load("readm", READM);
    let request = CallRequest::new("flip_thrice").actor(text("alice"));
    let response = served(engine.call(&request, &mut generator()).expect("call"));
    assert_eq!(
        row(&response).get("enabled").and_then(serde_json::Value::as_bool),
        Some(false),
        "the return observes the last write of the transition, not the first or the snapshot"
    );
}

/// A §5.2 computed value read through `$actor`: renaming the actor's `last`
/// changes `full = .first + ' ' + .last`, and the return recomputes it over the
/// resulting state — the value-level analogue of the corpus computed case, but
/// reached through `$actor` rather than a local/receiver.
#[test]
fn actor_return_recomputes_computed_over_resulting_state() {
    let mut engine = load("readm", READM);
    let request = CallRequest::new("rename").arg("last", text("Lovelace")).actor(text("alice"));
    let response = served(engine.call(&request, &mut generator()).expect("call"));
    assert_eq!(
        row(&response).get("full").and_then(serde_json::Value::as_str),
        Some("Ada Lovelace"),
        "$actor's computed `full` must derive from the resulting `last`, not the snapshot"
    );
}

/// Control: a program that writes a DIFFERENT row must leave `$actor`'s own
/// fields intact. alice disables bob; alice's `$actor.enabled` is still `true`.
/// Guards against the refresh conflating the actor with the touched row.
#[test]
fn actor_return_unaffected_by_change_to_another_row() {
    let mut engine = load("readm", READM);
    let request = CallRequest::new("disable_other").arg("id", text("bob")).actor(text("alice"));
    let response = served(engine.call(&request, &mut generator()).expect("call"));
    let row = row(&response);
    assert_eq!(row.get("id").and_then(serde_json::Value::as_str), Some("alice"));
    assert_eq!(
        row.get("enabled").and_then(serde_json::Value::as_bool),
        Some(true),
        "changing another account must not alter the actor's own returned fields"
    );
}
