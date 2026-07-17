#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.11 internal mutation calls, driven through the real engine. Each
//! expectation is re-derived from §8.11/§8.8/§22.2: a statement invoking a
//! declared mutation runs it inside the same atomic program, its writes are the
//! caller's, its argument object binds the callee's parameters, and a failure
//! inside the call rejects the caller's earlier writes too.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, RejectionReason};
use liasse_store::MemoryStore;
use liasse_value::Value;

use support::{generator, load};

/// A package with a failing internal call: `outer` first arms a flag, then calls
/// `fail_now`, whose assertion fails (the root is locked). §8.11/§22.2 make the
/// whole program reject, so the earlier flag write must not survive.
const NESTED_FAIL: &str = r#"{
  "$liasse": 1,
  "$app": "example.nestedfail@1.0.0",
  "$model": {
    "locked": "bool = true",
    "flags": { "$key": "id", "id": "text", "armed": "bool = false" },
    "flags_view": { "$view": ".flags { id, armed }" },
    "$mut": {
      "fail_now": "assert(!.locked, 'locked')",
      "outer": [ ".flags['f1'] { armed = true }", ".fail_now()" ]
    }
  },
  "$data": { "locked": true, "flags": { "f1": {} } }
}"#;

/// A package whose internal calls succeed and accumulate into the same commit:
/// `bump` increments a root counter, `add` adds its argument, and the callers
/// invoke them (with and without arguments) and return the resulting total.
const COUNTER: &str = r#"{
  "$liasse": 1,
  "$app": "example.counter@1.0.0",
  "$model": {
    "count": "int = 0",
    "count_view": { "$view": ".count" },
    "$mut": {
      "bump": ".count = .count + 1",
      "add({ n: int })": ".count = .count + @n",
      "bump_twice": [ ".bump()", ".bump()", "return .count" ],
      "add_two": [ ".add({ n: 3 })", ".add({ n: 4 })", "return .count" ]
    }
  },
  "$data": { "count": "0" }
}"#;

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut generator = generator();
    engine.call(request, &mut generator).expect("call")
}

/// The `armed` flag of `flags['f1']`.
fn armed(engine: &Engine<MemoryStore>) -> bool {
    let view = engine.view_at_head("flags_view").expect("view").expect("declared");
    match view.rows()[0].field("armed") {
        Some(Value::Bool(value)) => *value,
        other => panic!("expected a bool `armed`, got {other:?}"),
    }
}

/// §8.11/§8.8/§22.2: a failure inside an internal call rejects the caller's
/// earlier writes too — the flag armed before the failing call must roll back.
#[test]
fn internal_call_failure_rejects_caller_writes() {
    let mut engine = load("nested-fail", NESTED_FAIL);
    assert!(!armed(&engine), "the flag starts disarmed");

    let outcome = call(&mut engine, &CallRequest::new("outer"));
    match outcome {
        CallOutcome::Rejected(rejection) => {
            assert_eq!(rejection.reason(), RejectionReason::Assertion, "{rejection:?}");
        }
        other => panic!("expected an assertion rejection, got {other:?}"),
    }

    // The caller's earlier `armed = true` patch must not be visible.
    assert!(!armed(&engine), "the caller's write rolled back with the failed internal call");
}

/// §8.11: internal-call writes execute inside the same atomic program and
/// accumulate — two `.bump()` calls raise the counter to `2` in one commit.
#[test]
fn internal_call_writes_apply_in_same_transaction() {
    let mut engine = load("counter-bump", COUNTER);
    let outcome = call(&mut engine, &CallRequest::new("bump_twice"));
    match outcome {
        CallOutcome::Committed { response, .. } => {
            assert_eq!(response.expect("return").to_wire(), serde_json::json!("2"));
        }
        other => panic!("expected a committed counter of 2, got {other:?}"),
    }
}

/// §8.11: an internal call's argument object binds the callee's parameters —
/// `add({ n: 3 })` then `add({ n: 4 })` leaves the counter at `7`.
#[test]
fn internal_call_passes_arguments() {
    let mut engine = load("counter-add", COUNTER);
    let outcome = call(&mut engine, &CallRequest::new("add_two"));
    match outcome {
        CallOutcome::Committed { response, .. } => {
            assert_eq!(response.expect("return").to_wire(), serde_json::json!("7"));
        }
        other => panic!("expected a committed counter of 7, got {other:?}"),
    }
}
