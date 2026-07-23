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

use std::collections::BTreeMap;

use liasse_host::sim::{Behavior, SimNamespace};
use liasse_host::{
    FunctionDescriptor, HostNamespace, InvocationFailure, NamespaceDescriptor, NamespaceType,
};
use liasse_ident::InstanceId;
use liasse_runtime::{
    CallOutcome, CallRequest, ContractName, EffectClass, Engine, EngineError, FixedGenerators,
    InterfaceHash, OpSignature, Precision, Registry, RejectionReason, Version,
};
use liasse_store::MemoryStore;
use liasse_value::{Struct, StructType, Text, Type, Value};

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
    load_with_registry(def, registry())
}

fn load_with_registry(def: &str, registry: Registry) -> Result<Engine<MemoryStore>, EngineError> {
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = generator();
    Engine::load_with_hosts(store, def, &mut g, registry)
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

/// A verifier whose descriptor and implementation both require structured
/// arguments. Its accepted values are fixed independently of the mutation
/// implementation, so a committed result proves every nested parameter reached
/// the host call in the declared structural position.
struct StructuredVerifier {
    descriptor: NamespaceDescriptor,
}

impl StructuredVerifier {
    fn new() -> Self {
        let single = Type::Struct(StructType::new([("input".to_owned(), Type::Text)]));
        let multi = Type::Struct(StructType::new([
            ("public_key".to_owned(), Type::Text),
            ("message".to_owned(), Type::Text),
            ("signature".to_owned(), Type::Text),
        ]));
        let deep = Type::Struct(StructType::new([(
            "outer".to_owned(),
            Type::Set(Box::new(Type::Struct(StructType::new([(
                "inner".to_owned(),
                Type::Text,
            )])))),
        )]));
        let functions = [
            ("single".to_owned(), single),
            ("multi".to_owned(), multi),
            ("deep".to_owned(), deep),
        ]
        .into_iter()
        .map(|(name, argument)| {
            (
                name,
                FunctionDescriptor::new(
                    OpSignature::new([argument], Type::Text),
                    EffectClass::Verifier,
                ),
            )
        });
        Self {
            descriptor: NamespaceDescriptor::new(
                ContractName::parse("test.structured").expect("contract name"),
                Version::new(1, 0, 0),
                InterfaceHash::new("ih-structured-verifier-1"),
                BTreeMap::<String, NamespaceType>::new(),
                functions,
            ),
        }
    }

    fn expected_argument(function: &str) -> Option<Value> {
        match function {
            "single" => Some(Value::Struct(Struct::new([(
                Text::new("input"),
                Value::Text(Text::new("single-secret")),
            )]))),
            "multi" => Some(Value::Struct(Struct::new([
                (Text::new("public_key"), Value::Text(Text::new("pk-1"))),
                (Text::new("message"), Value::Text(Text::new("message-1"))),
                (
                    Text::new("signature"),
                    Value::Text(Text::new("signature-1")),
                ),
            ]))),
            "deep" => Some(Value::Struct(Struct::new([(
                Text::new("outer"),
                Value::Set(
                    [Value::Struct(Struct::new([(
                        Text::new("inner"),
                        Value::Text(Text::new("deep-secret")),
                    )]))]
                    .into_iter()
                    .collect(),
                ),
            )]))),
            _ => None,
        }
    }
}

impl HostNamespace for StructuredVerifier {
    fn descriptor(&self) -> &NamespaceDescriptor {
        &self.descriptor
    }

    fn invoke(&self, function: &str, args: &[Value]) -> Result<Value, InvocationFailure> {
        let expected = Self::expected_argument(function)
            .ok_or_else(|| InvocationFailure::UnknownFunction(function.to_owned()))?;
        let [actual] = args else {
            return Err(InvocationFailure::Arity {
                function: function.to_owned(),
                expected: 1,
                found: args.len(),
            });
        };
        if actual != &expected {
            return Err(InvocationFailure::Verification {
                detail: "structured credential is not accepted".to_owned(),
            });
        }
        Ok(Value::Text(Text::new(format!("{function}-proof"))))
    }
}

fn structured_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register_namespace(Box::new(StructuredVerifier::new()));
    registry
}

const NESTED_SINGLE_DEF: &str = r#"{
  "$liasse": 1,
  "$app": "t.nested_single@1.0.0",
  "$requires": { "auth": "test.structured@1" },
  "$model": {
    "sessions": { "$key": "id", "id": "text", "proof": "text" },
    "$mut": {
      "login": [
        "proof = auth.single({ input: @p })",
        "row = .sessions + { id: @id, proof: proof }",
        "return row { id, proof }"
      ]
    }
  }
}"#;

/// §8.3/§16.4: a parameter used only as an object field inside a host-call
/// argument is inferred from that descriptor field, binds, and reaches the host.
#[test]
fn nested_single_host_arg_param_binds_and_commits() {
    let mut engine = load_with_registry(NESTED_SINGLE_DEF, structured_registry()).expect("load");
    let request = CallRequest::new("login")
        .arg("id", Value::Text(Text::new("s1")))
        .arg("p", Value::Text(Text::new("single-secret")));
    let outcome = engine
        .call(&request, &mut generator())
        .expect("no engine fault");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("expected a committed structured login, got {outcome:?}");
    };
    assert_eq!(
        response.expect("return value").to_wire(),
        serde_json::json!({ "id": "s1", "proof": "single-proof" }),
    );
}

const NESTED_MULTI_DEF: &str = r#"{
  "$liasse": 1,
  "$app": "t.nested_multi@1.0.0",
  "$requires": { "auth": "test.structured@1" },
  "$model": {
    "sessions": { "$key": "id", "id": "text", "proof": "text" },
    "$mut": {
      "login": [
        "proof = auth.multi({ public_key: @a, message: @b, signature: @c })",
        "row = .sessions + { id: @id, proof: proof }",
        "return row { id, proof }"
      ]
    }
  }
}"#;

/// The Bilani verifier shape: all three parameters occur only in one structured
/// host argument. Accepted values commit the proof; a rejected credential leaves
/// the already-committed frontier unchanged.
#[test]
fn nested_multi_host_arg_params_bind_and_rejection_is_atomic() {
    let mut engine = load_with_registry(NESTED_MULTI_DEF, structured_registry()).expect("load");
    let accepted = CallRequest::new("login")
        .arg("id", Value::Text(Text::new("s1")))
        .arg("a", Value::Text(Text::new("pk-1")))
        .arg("b", Value::Text(Text::new("message-1")))
        .arg("c", Value::Text(Text::new("signature-1")));
    let outcome = engine
        .call(&accepted, &mut generator())
        .expect("no engine fault");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("expected a committed multi-field login, got {outcome:?}");
    };
    assert_eq!(
        response.expect("return value").to_wire(),
        serde_json::json!({ "id": "s1", "proof": "multi-proof" }),
    );

    let head_before = engine.head().expect("head");
    let rejected = CallRequest::new("login")
        .arg("id", Value::Text(Text::new("s2")))
        .arg("a", Value::Text(Text::new("pk-1")))
        .arg("b", Value::Text(Text::new("message-1")))
        .arg("c", Value::Text(Text::new("wrong-signature")));
    let outcome = engine
        .call(&rejected, &mut generator())
        .expect("no engine fault");
    let CallOutcome::Rejected(rejection) = outcome else {
        panic!("expected a verifier rejection, got {outcome:?}");
    };
    assert_eq!(rejection.reason(), RejectionReason::Host);
    assert_eq!(
        engine.head().expect("head"),
        head_before,
        "a rejected verifier commits nothing"
    );
}

const NESTED_DEEP_DEF: &str = r#"{
  "$liasse": 1,
  "$app": "t.nested_deep@1.0.0",
  "$requires": { "auth": "test.structured@1" },
  "$model": {
    "sessions": { "$key": "id", "id": "text", "proof": "text" },
    "$mut": {
      "login": [
        "proof = auth.deep({ outer: [{ inner: @p }] })",
        "row = .sessions + { id: @id, proof: proof }",
        "return row { id, proof }"
      ]
    }
  }
}"#;

/// Object → array/set → object nesting proves the collector and descriptor
/// typing recurse through arbitrary structured combinations, not one object
/// level alone.
#[test]
fn deeply_nested_array_host_arg_param_binds_and_commits() {
    let mut engine = load_with_registry(NESTED_DEEP_DEF, structured_registry()).expect("load");
    let request = CallRequest::new("login")
        .arg("id", Value::Text(Text::new("s1")))
        .arg("p", Value::Text(Text::new("deep-secret")));
    let outcome = engine
        .call(&request, &mut generator())
        .expect("no engine fault");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("expected a committed deeply nested login, got {outcome:?}");
    };
    assert_eq!(
        response.expect("return value").to_wire(),
        serde_json::json!({ "id": "s1", "proof": "deep-proof" }),
    );
}
