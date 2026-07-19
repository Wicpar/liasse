#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §16.2/§16.3 host-namespace calls in *expression* positions, resolved and
//! evaluated through the typed expression checker and the environment's host-call
//! hook — the seam distinct from the interpreter's `name = ns.fn(...)` binding
//! path.
//!
//! # Reachability across the liasse-model seam
//!
//! A host call in a *view*, *field default*, or *computed value* is type-checked
//! by liasse-model's Phase-2 `check_tree` (`ModelScope`). `Engine::load_with_hosts`
//! resolves the package's `$requires` against the registry and threads the pinned
//! signatures into `Model::build_with_hosts`, so `ModelScope` now types such a call
//! against its contract and effect policy (§16.3) instead of rejecting it as an
//! "unknown function" before activation — closing the seam this file once tracked.
//! The `$mut` operator-value position (an insert object member `{ field: ns.fn(args) }`)
//! is still accepted structurally by the model (the whole insert
//! `uses_mutation_operator`) and type-checked by the runtime's compiled layer.
//! These tests drive both: the mutation-value path and the view/default positions
//! end to end through `Engine::load_with_hosts`.
//!
//! Every expectation is re-derived from the sim namespace's pinned behaviour
//! (`double: x -> 2x` pure; `token` generated; `bad` a `(int) -> int` returning a
//! `text`), not from the implementation.

use liasse_host::sim::{Behavior, SimNamespace};
use liasse_ident::InstanceId;
use liasse_runtime::{
    CallOutcome, CallRequest, ContractName, EffectClass, Engine, EngineError, FixedGenerators,
    InterfaceHash, OpSignature, Precision, Registry, RejectionReason, Version,
};
use liasse_store::MemoryStore;
use liasse_value::{Integer, Text, Type, Value};

const NOW: i128 = 1_700_000_000_000_000;

fn generator() -> FixedGenerators {
    FixedGenerators::new(NOW, Precision::Micros)
}

/// A `test.util@1` namespace declaring a pure `double`, a generated `token`, and a
/// nonconforming pure `bad` (declared `(int) -> int`, returns a `text`).
fn util_namespace() -> SimNamespace {
    SimNamespace::builder(
        ContractName::parse("test.util").expect("contract name"),
        Version::new(1, 2, 0),
        InterfaceHash::new("ih-util-1"),
    )
    .function("double", OpSignature::new([Type::Int], Type::Int), EffectClass::Pure, Behavior::Double)
    .function("token", OpSignature::new([], Type::Text), EffectClass::Generated, Behavior::Token)
    .function("bad", OpSignature::new([Type::Int], Type::Int), EffectClass::Pure, Behavior::OffType)
    .build()
}

fn util_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register_namespace(Box::new(util_namespace()));
    registry
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

fn load(def: &str) -> Result<Engine<MemoryStore>, EngineError> {
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = generator();
    Engine::load_with_hosts(store, def, &mut g, util_registry())
}

/// §16.2/§16.3: a *pure* host call in an insert object member (a write position)
/// type-checks against the pinned `(int) -> int` signature and evaluates through
/// the environment's host-call hook; `double(21) = 42` flows into committed state.
#[test]
fn pure_host_call_in_an_insert_member_flows_into_state() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.pureinsert@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "items": { "$key": "id", "id": "text", "doubled": "int = 0" },
        "$mut": {
          "add({ id: text, x: int })": [
            "row = .items + { id: @id, doubled: util.double(@x) }",
            "return row { id, doubled }"
          ]
        }
      }
    }"#;
    let mut engine = load(def).expect("load resolves util");
    let mut g = generator();
    let request = CallRequest::new("add").arg("id", Value::Text(Text::new("r1"))).arg("x", int(21));
    let outcome = engine.call(&request, &mut g).expect("no engine fault");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("expected a committed insert, got {outcome:?}");
    };
    let wire = response.expect("a return value").to_wire();
    assert_eq!(wire, serde_json::json!({ "id": "r1", "doubled": "42" }));
}

/// §16.3/§8.8: a *generated* host call is admissible in a write position and its
/// value flows into state. `token` at the sim's initial phase is `tok-0`,
/// deducible from the double's pinned behaviour.
#[test]
fn generated_host_call_in_a_write_position_yields_its_value() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.geninsert@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "items": { "$key": "id", "id": "text", "tok": "text = ''" },
        "$mut": {
          "add({ id: text })": [
            "row = .items + { id: @id, tok: util.token() }",
            "return row { id, tok }"
          ]
        }
      }
    }"#;
    let mut engine = load(def).expect("load");
    let mut g = generator();
    let request = CallRequest::new("add").arg("id", Value::Text(Text::new("r1")));
    let outcome = engine.call(&request, &mut g).expect("no engine fault");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("expected a committed insert, got {outcome:?}");
    };
    let wire = response.expect("a return value").to_wire();
    assert_eq!(wire, serde_json::json!({ "id": "r1", "tok": "tok-0" }));
}

/// §16.2/§16.3 (SPEC-ISSUES item 15): a component returning an off-contract type
/// through a host call in an expression position is caught by the conformance
/// guard — the mutation is rejected as a host refusal with no committed effect,
/// exactly as the mutation-program call path is guarded.
#[test]
fn nonconforming_return_in_an_expression_call_is_caught() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.badinsert@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "items": { "$key": "id", "id": "text", "n": "int = 0" },
        "$mut": {
          "add({ id: text, x: int })": [
            "row = .items + { id: @id, n: util.bad(@x) }",
            "return row { id, n }"
          ]
        }
      }
    }"#;
    let mut engine = load(def).expect("load — the signature is well-typed; the drift is a run-time breach");
    let mut g = generator();
    let head_before = engine.head().unwrap();
    let request = CallRequest::new("add").arg("id", Value::Text(Text::new("r1"))).arg("x", int(3));
    let outcome = engine.call(&request, &mut g).expect("no engine fault");
    let CallOutcome::Rejected(rejection) = outcome else {
        panic!("expected the guard to reject a nonconforming return, got {outcome:?}");
    };
    assert_eq!(rejection.reason(), RejectionReason::Host);
    assert_eq!(engine.head().unwrap(), head_before, "the rejected insert commits nothing");
}

/// §16.5 (mandate 7): a *pure* app-registered host call in a `$view` (a
/// database-evaluated position) is rejected at load — a `$requires` namespace is
/// legal only inside a mutation program, so the package fails to type-check before
/// activation even though the function's effect class is pure. The escape is to
/// compute the value in a mutation body and store it, then read the stored field
/// in the view (exercised by `pure_host_call_in_an_insert_member_flows_into_state`).
#[test]
fn pure_host_call_in_a_view_rejects_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.pureview@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "items": { "$key": "id", "id": "text", "n": "int" },
        "doubled": { "$view": ".items { id, d: util.double(.n) }" }
      },
      "$data": { "items": { "r1": { "n": 21 } } }
    }"#;
    match load(def) {
        Err(EngineError::Invalid(_)) => {}
        Err(_) => panic!("expected a static Invalid rejection, got a different engine error"),
        Ok(_) => panic!("expected §16.5 to reject an app namespace call in a view, but it loaded"),
    }
}

/// §16.3/§8.8: a *generated* host call in a `$view` (a pure read position) is
/// rejected at load — `token` is generated and only a pure function may run in a
/// view, so the package fails to type-check before activation.
#[test]
fn generated_host_call_in_a_pure_view_rejects_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.genview@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "minted": { "$view": "util.token()" }
      }
    }"#;
    match load(def) {
        Err(EngineError::Invalid(_)) => {}
        Err(_) => panic!("expected a static Invalid rejection, got a different engine error"),
        Ok(_) => panic!("expected a static rejection of a generated call in a pure view, but it loaded"),
    }
}

/// §16.2: a host call naming a namespace with no `$requires` declaration fails to
/// type-check where the checker resolves it — an insert member calling `cbor.encode`
/// with no requirement rejects the mutation (availability in the registry does not
/// substitute for the explicit package requirement). The declared `util` requirement
/// is exercised (`util.double`) so §16.2's used-requirement rule is satisfied and the
/// package loads; only the undeclared `cbor` call is under test.
#[test]
fn undeclared_namespace_call_in_an_insert_member_rejects() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.undeclared@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "items": { "$key": "id", "id": "text", "n": "int = 0" },
        "$mut": {
          "add({ id: text, x: int })": [
            "row = .items + { id: @id, n: cbor.encode(util.double(@x)) }",
            "return row { id, n }"
          ]
        }
      }
    }"#;
    let mut engine = load(def).expect("load — the undeclared call is only reached at admission");
    let mut g = generator();
    let request = CallRequest::new("add").arg("id", Value::Text(Text::new("r1"))).arg("x", int(3));
    let outcome = engine.call(&request, &mut g).expect("no engine fault");
    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "an undeclared namespace call must reject the admission, got {outcome:?}",
    );
}
