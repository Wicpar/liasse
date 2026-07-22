#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.3/§11.5/§16.4: a mutation parameter used *only* as a host-namespace call
//! argument (`ns.verify(@credential)`) is a real declared contract parameter.
//!
//! This is the login-blocker shape: an auth mutation verifies an external proof
//! through a registered verifier namespace (`webauthn.verify(@response)`,
//! `password.verify(@credential)`, `oidc.verify(@response)` — §11.5) whose
//! argument is the mutation's `@credential`/`@response` parameter and appears
//! nowhere else. §8.3 infers such a parameter from the host function's declared
//! argument signature so the caller passes it explicitly in the §12.1 closed
//! argument object; before this fix the parameter was dropped from the contract,
//! the closed-argument check rejected the caller's `credential`, and the host
//! call read an unbound `@credential` — login failed both ways.
//!
//! Expectations are re-derived from the sim namespace's pinned behaviour
//! (`verify: text credential -> mapped proof`, a verifier), not from the
//! implementation.

use liasse_host::sim::{Behavior, SimNamespace};
use liasse_ident::InstanceId;
use liasse_runtime::{
    CallOutcome, CallRequest, ContractName, EffectClass, Engine, EngineError, FixedGenerators,
    InterfaceHash, OpSignature, Precision, Registry, RejectionReason, Version,
};
use liasse_store::MemoryStore;
use liasse_value::{Text, Type, Value};

const NOW: i128 = 1_700_000_000_000_000;

fn generator() -> FixedGenerators {
    FixedGenerators::new(NOW, Precision::Micros)
}

/// A `test.verifier@1` namespace declaring a `verify` verifier: it maps an
/// accepted `text` credential to the `text` proof `identity-42`.
fn verifier_namespace() -> SimNamespace {
    SimNamespace::builder(
        ContractName::parse("test.verifier").expect("contract name"),
        Version::new(1, 0, 0),
        InterfaceHash::new("ih-verifier-1"),
    )
    .function(
        "verify",
        OpSignature::new([Type::Text], Type::Text),
        EffectClass::Verifier,
        Behavior::Accept,
    )
    .accepts("good-secret", Value::Text(Text::new("identity-42")))
    .build()
}

fn registry() -> Registry {
    let mut registry = Registry::new();
    registry.register_namespace(Box::new(verifier_namespace()));
    registry
}

fn load(def: &str) -> Result<Engine<MemoryStore>, EngineError> {
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = generator();
    Engine::load_with_hosts(store, def, &mut g, registry())
}

/// The §11.5 login shape: `@credential`'s ONLY use is the host-call argument
/// `auth.verify(@credential)`, with NO explicit prototype. §8.3 must infer it
/// (as `text`, from `verify`'s declared signature) into the contract so the
/// package loads and the caller can supply it.
const LOGIN_DEF: &str = r#"{
  "$liasse": 1,
  "$app": "t.login@1.0.0",
  "$requires": { "auth": "test.verifier@1" },
  "$model": {
    "sessions": { "$key": "id", "id": "text", "identity": "text = ''" },
    "$mut": {
      "login": [
        "identity = auth.verify(@credential)",
        "row = .sessions + { id: @id, identity: identity }",
        "return row { id, identity }"
      ]
    }
  }
}"#;

/// §8.3/§16.4: the package LOADS — `@credential`, used only inside
/// `auth.verify(@credential)`, is inferred into the parameter contract from the
/// host function's declared `(text) -> text` signature.
#[test]
fn host_arg_only_param_loads() {
    load(LOGIN_DEF).expect("§8.3 infers a host-call-argument parameter into the contract");
}

/// §11.5/§12.1: the caller supplies `credential` in the closed argument object;
/// the now-declared parameter binds and reaches `auth.verify(@credential)`,
/// which returns the mapped proof `identity-42` that flows into committed state.
#[test]
fn host_arg_only_param_binds_the_supplied_value() {
    let mut engine = load(LOGIN_DEF).expect("load");
    let mut g = generator();
    let request = CallRequest::new("login")
        .arg("id", Value::Text(Text::new("s1")))
        .arg("credential", Value::Text(Text::new("good-secret")));
    let outcome = engine.call(&request, &mut g).expect("no engine fault");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("expected a committed login, got {outcome:?}");
    };
    let wire = response.expect("a return value").to_wire();
    assert_eq!(wire, serde_json::json!({ "id": "s1", "identity": "identity-42" }));
}

/// §16.3: the verifier rejects an unaccepted credential; the bound `@credential`
/// reaches the host call (proving it is a real contract parameter) and the
/// verifier's rejection commits no effect.
#[test]
fn host_arg_only_param_rejected_credential_commits_nothing() {
    let mut engine = load(LOGIN_DEF).expect("load");
    let mut g = generator();
    let head_before = engine.head().unwrap();
    let request = CallRequest::new("login")
        .arg("id", Value::Text(Text::new("s1")))
        .arg("credential", Value::Text(Text::new("wrong-secret")));
    let outcome = engine.call(&request, &mut g).expect("no engine fault");
    let CallOutcome::Rejected(rejection) = outcome else {
        panic!("expected a verifier rejection, got {outcome:?}");
    };
    assert_eq!(rejection.reason(), RejectionReason::Host);
    assert_eq!(engine.head().unwrap(), head_before, "a rejected login commits nothing");
}
