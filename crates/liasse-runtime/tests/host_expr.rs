#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §16.2/§16.3 host-namespace calls in *expression* positions, resolved and
//! evaluated through the typed expression checker and the environment's host-call
//! hook — the seam distinct from the interpreter's `name = ns.fn(...)` binding
//! path.
//!
//! # Reachability and the liasse-model seam
//!
//! A host call in a *view*, *field default*, or *computed value* is type-checked
//! by liasse-model's Phase-2 `check_tree` (`ModelScope`), which does not resolve
//! `$requires` descriptors, so such a package is rejected at `Model::build` as an
//! "unknown function" *before* the runtime's checker runs. Closing that requires a
//! liasse-model change (thread the descriptors into `ModelScope`, mirroring how
//! the mutation checker's `type_value` already skips `is_program_call` host
//! calls). Until then, the reachable expression position is a `$mut` operator
//! value — an insert object member `{ field: ns.fn(args) }` — which the model
//! accepts structurally (the whole insert `uses_mutation_operator`) and the
//! runtime type-checks against the resolved signatures and evaluates through the
//! environment's host-call hook. These tests drive that path; the view/default
//! positions are exercised at the checker level in `liasse-expr`.
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
    let head_before = engine.head();
    let request = CallRequest::new("add").arg("id", Value::Text(Text::new("r1"))).arg("x", int(3));
    let outcome = engine.call(&request, &mut g).expect("no engine fault");
    let CallOutcome::Rejected(rejection) = outcome else {
        panic!("expected the guard to reject a nonconforming return, got {outcome:?}");
    };
    assert_eq!(rejection.reason(), RejectionReason::Host);
    assert_eq!(engine.head(), head_before, "the rejected insert commits nothing");
}

/// §16.2: a host call naming a namespace with no `$requires` declaration fails to
/// type-check where the checker resolves it — an insert member calling `cbor.encode`
/// with no requirement rejects the mutation (availability in the registry does not
/// substitute for the explicit package requirement).
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
            "row = .items + { id: @id, n: cbor.encode(@x) }",
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
