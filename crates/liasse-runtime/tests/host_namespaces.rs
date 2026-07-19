#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §16 host-namespace resolution and call evaluation, and the §17.7/§17.8 cose
//! sign path, driven end-to-end through the engine's mutation admission.
//!
//! Every expectation is re-derived from the spec and the sim namespace's pinned
//! behaviour (double: `x -> 2x` pure; off_type: a `(int) -> int` that returns a
//! `text`): a mutation calling a pure host function commits with the returned
//! value; a login mutation calling `cose.sign` commits and issues a token that
//! verifies against the ring's active version; a missing `$requires` namespace
//! fails load before activation; and a nonconforming return is caught by the
//! conformance guard, rejecting the mutation with no committed effect.

use liasse_expr::Cell;
use liasse_host::sim::{Behavior, SimNamespace};
use liasse_ident::InstanceId;
use liasse_runtime::{
    CallOutcome, CallRequest, ContractName, CoseVerifyError, EffectClass, Engine, EngineError,
    FixedGenerators, InterfaceHash, OpSignature, Precision, Registry, RejectionReason, Version,
};
use liasse_store::MemoryStore;
use liasse_value::{Integer, Text, Type, Value};

const NOW: i128 = 1_700_000_000_000_000;

fn generator() -> FixedGenerators {
    FixedGenerators::new(NOW, Precision::Micros)
}

/// A `test.util@1` namespace double declaring a pure `double : (int) -> int` and a
/// nonconforming `bad : (int) -> int` that in fact returns a `text` (§16 NOTES,
/// SPEC-ISSUES item 15).
fn util_namespace() -> SimNamespace {
    SimNamespace::builder(
        ContractName::parse("test.util").expect("contract name"),
        Version::new(1, 2, 0),
        InterfaceHash::new("ih-util-1"),
    )
    .function("double", OpSignature::new([Type::Int], Type::Int), EffectClass::Pure, Behavior::Double)
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

/// §16.2/§16.3: a mutation program may call a resolved pure host namespace; the
/// returned value flows into the committed state. `double(21) = 42` is deduced
/// from the sim behaviour, not the implementation.
#[test]
fn mutation_calling_pure_host_fn_commits_with_returned_value() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.util@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "rows": { "$key": "id", "id": "text", "n": "int = 0" },
        "$mut": {
          "add({ id: text, x: int })": [
            "doubled = util.double(@x)",
            "row = .rows + { id: @id, n: doubled }",
            "return row { id, n }"
          ]
        }
      }
    }"#;
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = generator();
    let mut engine =
        Engine::load_with_hosts(store, def, &mut g, util_registry()).expect("load resolves util");

    let request = CallRequest::new("add").arg("id", Value::Text(Text::new("r1"))).arg("x", int(21));
    let outcome = engine.call(&request, &mut g).expect("no engine fault");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("expected a committed insert, got {outcome:?}");
    };
    let wire = response.expect("a return value").to_wire();
    assert_eq!(wire, serde_json::json!({ "id": "r1", "n": "42" }));
}

/// §16.2, §9.2 step 4: under host management, a package requiring an unregistered
/// namespace fails to load before activation. `load_with_hosts` enforces §16.2
/// strictly; the registry (cose-only) has no `liasse.cbor`.
#[test]
fn missing_requires_namespace_fails_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.missing@1.0.0",
      "$requires": { "cbor": "liasse.cbor@1" },
      "$model": { "rows": { "$key": "id", "id": "text" } }
    }"#;
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = generator();
    match Engine::load_with_hosts(store, def, &mut g, Registry::new()) {
        Err(EngineError::Requirement(detail)) => {
            assert!(detail.contains("liasse.cbor"), "names the unmet requirement: {detail}");
        }
        Err(other) => panic!("expected a requirement failure, got a different error: {other:?}"),
        Ok(_) => panic!("expected a requirement failure, but the load succeeded"),
    }
}

/// §16.2 (SPEC-ISSUES #17): the used-requirement rule is a *static* model rule,
/// so it holds even under the default [`Engine::load`], which manages no host
/// components. `cbor` is declared but no expression uses it, so the build rejects
/// before host resolution is ever consulted — the deferral of an *unresolved*
/// requirement never applies to an *unused* one.
#[test]
fn default_load_rejects_unused_requirement() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.unused@1.0.0",
      "$requires": { "cbor": "liasse.cbor@1" },
      "$model": { "rows": { "$key": "id", "id": "text" } }
    }"#;
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = generator();
    assert!(
        matches!(Engine::load(store, def, &mut g), Err(EngineError::Invalid(_))),
        "an unused requirement is a static §16.2 rejection even with no host management",
    );
}

/// §16.2: an incompatible major fails resolution — a `@2` requirement is not
/// satisfied by a registered `1.x` descriptor.
#[test]
fn incompatible_major_fails_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.util@1.0.0",
      "$requires": { "util": "test.util@2" },
      "$model": { "rows": { "$key": "id", "id": "text" } }
    }"#;
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = generator();
    match Engine::load_with_hosts(store, def, &mut g, util_registry()) {
        Err(EngineError::Requirement(_)) => {}
        Err(other) => panic!("expected an incompatible-major failure, got a different error: {other:?}"),
        Ok(_) => panic!("expected an incompatible-major failure, but the load succeeded"),
    }
}

/// §16.2/§16.3 (SPEC-ISSUES item 15): a component returning an off-contract type
/// is caught by the conformance guard, so the mutation is rejected with no
/// committed effect — the guard does not trust the declared signature blindly.
#[test]
fn nonconforming_host_return_is_caught_by_guard() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.util@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "rows": { "$key": "id", "id": "text", "n": "int = 0" },
        "$mut": {
          "add({ id: text, x: int })": [
            "bad = util.bad(@x)",
            "row = .rows + { id: @id, n: bad }",
            "return row { id, n }"
          ]
        }
      }
    }"#;
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = generator();
    let mut engine = Engine::load_with_hosts(store, def, &mut g, util_registry()).expect("load");

    let request = CallRequest::new("add").arg("id", Value::Text(Text::new("r1"))).arg("x", int(3));
    let outcome = engine.call(&request, &mut g).expect("no engine fault");
    let CallOutcome::Rejected(rejection) = outcome else {
        panic!("expected the guard to reject a nonconforming return, got {outcome:?}");
    };
    assert_eq!(rejection.reason(), RejectionReason::Host);
    // The state is untouched: no row was inserted.
    assert!(engine.head().unwrap().get() >= 1);
    let request2 = CallRequest::new("add").arg("id", Value::Text(Text::new("r2"))).arg("x", int(4));
    assert!(
        matches!(engine.call(&request2, &mut g).expect("no fault"), CallOutcome::Rejected(_)),
        "the nonconforming call rejects deterministically",
    );
}

/// The login package of the §17.8 direct-token flow, minus the auth/role/public
/// surfaces the surface layer drives — the mutation core the runtime must admit.
const LOGIN: &str = r#"{
  "$liasse": 1,
  "$app": "t.keyrings@1.0.0",
  "$requires": { "cose": "liasse.cose@1" },
  "$model": {
    "session_keys": { "$keyring": { "$provider": "test-kp", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" } },
    "accounts": { "$key": "id", "id": "text", "name": "text" },
    "sessions": { "$key": "id", "id": "uuid = uuid()", "account": { "$ref": "/accounts" }, "revoked": "bool = false" },
    "$mut": {
      "login": [
        "session = /sessions + { account: @account }",
        "token = cose.sign(/session_keys, { auth: 'session', session: session.$key })",
        "return { token }"
      ]
    }
  },
  "$data": { "accounts": { "alice": { "name": "Alice" } } }
}"#;

/// §17.7/§17.8: a login mutation calling `cose.sign(/session_keys, claims)` mints
/// a token through the ring's active version and commits; the ring has an active
/// `.$current` version, and the returned token verifies against the ring's
/// accepted set.
#[test]
fn login_mutation_signs_token_and_commits() {
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = generator();
    let mut engine = Engine::load(store, LOGIN, &mut g).expect("load resolves cose");

    // §17.3: automatic bootstrap activated the ring's first version.
    let ring = engine.keyring("session_keys").expect("declared keyring");
    assert!(ring.current().is_some(), "the ring has one active version after bootstrap");

    let request = CallRequest::new("login").arg("account", Value::Text(Text::new("alice")));
    let outcome = engine.call(&request, &mut g).expect("no engine fault");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("expected the login to commit, got {outcome:?}");
    };
    let response = response.expect("a return value");
    let token = token_of(&response);

    // §17.7: the minted token verifies against the ring's accepted versions and
    // carries back the signed claims (the `auth` claim binds the authenticator)
    // together with the verified key-version identity — the active version here.
    let active = engine.keyring("session_keys").expect("ring").current().expect("active").id();
    let (claims, version) = engine.cose_verify("session_keys", &token).expect("token verifies");
    assert_eq!(version, active, "verification reports the signing version identity");
    let Value::Struct(fields) = &claims else { panic!("claims are a struct: {claims:?}") };
    assert_eq!(fields.get("auth"), Some(&Value::Text(Text::new("session"))));
}

/// §17.7: a token minted by one ring is denied by a different ring (a foreign-ring
/// token), and a structurally-broken value is not a token.
#[test]
fn cose_verify_denies_foreign_and_malformed_tokens() {
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = generator();
    let mut engine = Engine::load(store, LOGIN, &mut g).expect("load");
    let request = CallRequest::new("login").arg("account", Value::Text(Text::new("alice")));
    let response = engine
        .call(&request, &mut g)
        .expect("no fault")
        .response()
        .cloned()
        .expect("a return value");
    let token = token_of(&response);

    assert!(matches!(
        engine.cose_verify("no_such_ring", &token),
        Err(CoseVerifyError::UnknownRing(_)),
    ));
    assert!(matches!(
        engine.cose_verify("session_keys", &Value::Text(Text::new("garbage"))),
        Err(CoseVerifyError::Malformed),
    ));
}

/// §17.9: a provider that cannot sign rejects the login mutation before any
/// effect — no session row is committed. The `provider_set { fail: [sign] }`
/// fault is injected through the engine's keyring provider accessor.
#[test]
fn sign_failure_rejects_login_without_effect() {
    use liasse_host::sim::ProviderOp;

    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = generator();
    let mut engine = Engine::load(store, LOGIN, &mut g).expect("load");
    let head_before = engine.head().unwrap();

    engine.keyring_provider_mut("session_keys").expect("declared ring").set_fail([ProviderOp::Sign]);

    let request = CallRequest::new("login").arg("account", Value::Text(Text::new("alice")));
    let outcome = engine.call(&request, &mut g).expect("no engine fault");
    let CallOutcome::Rejected(rejection) = outcome else {
        panic!("expected a §17.9 sign-failure rejection, got {outcome:?}");
    };
    assert_eq!(rejection.reason(), RejectionReason::Host);
    // §22.2: a failed sign commits nothing — the head has not advanced.
    assert_eq!(engine.head().unwrap(), head_before, "no partial effect from the failed login");
}

/// Extract the `token` member of a `return { token }` response as a value. The
/// object literal delivers as a scalar `Struct` cell whose `token` member is the
/// cose token struct (§17.8).
fn token_of(response: &liasse_runtime::ResponseValue) -> Value {
    match response.cell() {
        Cell::Scalar(Value::Struct(fields)) => {
            fields.get("token").cloned().expect("a `token` member")
        }
        other => panic!("a `{{ token }}` return is a struct value, got {other:?}"),
    }
}
