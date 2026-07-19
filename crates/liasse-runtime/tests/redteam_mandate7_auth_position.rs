#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM — Phase-7b mandate-7 (§16.5 execution contexts) adversarial suite:
//! the authenticator-expression positions.
//!
//! §16.5 (and §11.3, as amended): the authenticator expressions `$verify`,
//! `$actor`, `$session`, and `$check` are DATABASE-EVALUATED positions — they are
//! restricted to the language operators, the built-in namespaces (§16.1), and
//! native keyring verification (§17.7). A call to a `$requires`-registered
//! namespace in any of them is a LOAD-TIME error; custom credential verification
//! re-models onto the §11.5 auth-mutation pattern.
//!
//! FINDING (HIGH — wiring hole): none of these four positions is host-checked on
//! the real load/admission path. `liasse-model` `auth.rs` only `parse_only`s them
//! (syntax, no typing), and the runtime `CompiledPackage::build` never compiles
//! the authenticator expressions at all (`AuthBindings::derive` resolves only the
//! `$actor`/`$session` COLLECTION paths, not the expressions). So an
//! app-registered namespace call in `$verify`/`$actor`/`$session`/auth-`$check`
//! is ADMITTED at load through `Engine::load_with_hosts` — the very "runtime auth
//! path" the model static runner's skip-list (`corpus_static.rs`) claims enforces
//! the rule. It does not.
//!
//! Root cause: `crates/liasse-model/src/auth.rs:95,97,100,139,147` (`parse_only`
//! for `$verify`/`$session`/`$actor`/`$check`), plus the absence of any
//! authenticator-expression compile in `crates/liasse-runtime/src/compiled.rs`
//! `CompiledPackage::build` / `AuthBindings` (compiled.rs:1041-1055).
//!
//! Each FINDING test asserts the SPEC-CORRECT outcome (load rejected) and so
//! FAILS against the current build, marking the hole. The CONTROL tests assert
//! the positions that ARE wired, and PASS — isolating the defect to the auth
//! positions.

use liasse_host::sim::{Behavior, SimNamespace};
use liasse_ident::InstanceId;
use liasse_runtime::{
    ContractName, EffectClass, Engine, EngineError, FixedGenerators, InterfaceHash, OpSignature,
    Precision, Registry, Version,
};
use liasse_store::MemoryStore;
use liasse_value::Type;

const NOW: i128 = 1_700_000_000_000_000;

fn generator() -> FixedGenerators {
    FixedGenerators::new(NOW, Precision::Micros)
}

/// A `test.auth@1` namespace declaring a `verifier` `check` and a `pure`
/// `resolve`, both `(text) -> json`. `resolve` isolates the §16.5 ORIGIN rule
/// (a pure app fn) from the §16.3 effect rule; `check` is the natural verifier.
fn auth_registry() -> Registry {
    let ns = SimNamespace::builder(
        ContractName::parse("test.auth").expect("contract name"),
        Version::new(1, 0, 0),
        InterfaceHash::new("ih-auth-1"),
    )
    .function(
        "check",
        OpSignature::new([Type::Text], Type::Json),
        EffectClass::Verifier,
        Behavior::Accept,
    )
    .function(
        "resolve",
        OpSignature::new([Type::Text], Type::Json),
        EffectClass::Pure,
        Behavior::Accept,
    )
    .build();
    let mut registry = Registry::new();
    registry.register_namespace(Box::new(ns));
    registry
}

/// A `test.util@1` namespace with a `pure` `double : (int) -> int`, for the
/// non-auth controls.
fn util_registry() -> Registry {
    let ns = SimNamespace::builder(
        ContractName::parse("test.util").expect("contract name"),
        Version::new(1, 2, 0),
        InterfaceHash::new("ih-util-1"),
    )
    .function(
        "double",
        OpSignature::new([Type::Int], Type::Int),
        EffectClass::Pure,
        Behavior::Double,
    )
    .build();
    let mut registry = Registry::new();
    registry.register_namespace(Box::new(ns));
    registry
}

fn load(def: &str, registry: Registry) -> Result<Engine<MemoryStore>, EngineError> {
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = generator();
    Engine::load_with_hosts(store, def, &mut g, registry)
}

// ---------------------------------------------------------------------------
// FINDINGS (expected to FAIL until the auth positions are host-checked at load)
// ---------------------------------------------------------------------------

/// §16.5/§11.3/§16.3: an app-registered VERIFIER call in `$verify` is a load
/// error (verifier effect in a db-evaluated position AND app origin outside a
/// mutation body). This is the exact `app-verifier-in-dollar-verify-rejected`
/// corpus package, driven through the runtime auth load path.
#[test]
fn app_verifier_in_dollar_verify_must_be_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.v@1.0.0",
      "$requires": { "authns": "test.auth@1" },
      "$model": {
        "accounts": { "$key": "id", "id": "text", "name": "text" },
        "$auth": { "token": {
          "$credential": "text",
          "$verify": "authns.check($credential)",
          "$actor": "/accounts[$proof.account]"
        } }
      }
    }"#;
    assert!(
        load(def, auth_registry()).is_err(),
        "FINDING (HIGH): §16.5/§11.3 requires an app-registered verifier call in `$verify` to be \
         a LOAD-TIME error, but the package loaded — `$verify` is never host-checked (model \
         auth.rs parse_only; runtime never compiles the authenticator expressions)",
    );
}

/// §16.5 (origin, isolated): even a PURE app fn in `$verify` is a load error —
/// the origin rule is independent of the effect class.
#[test]
fn app_pure_fn_in_dollar_verify_must_be_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.v@1.0.0",
      "$requires": { "authns": "test.auth@1" },
      "$model": {
        "accounts": { "$key": "id", "id": "text", "name": "text" },
        "$auth": { "token": {
          "$credential": "text",
          "$verify": "authns.resolve($credential)",
          "$actor": "/accounts[$proof.account]"
        } }
      }
    }"#;
    assert!(
        load(def, auth_registry()).is_err(),
        "FINDING (HIGH): §16.5 makes an app-registered namespace call in `$verify` a LOAD-TIME \
         error regardless of effect class, but the package with a pure `authns.resolve` in \
         `$verify` loaded",
    );
}

/// §16.5: an app-registered call in `$actor` is a load error.
#[test]
fn app_fn_in_dollar_actor_must_be_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.a@1.0.0",
      "$requires": { "authns": "test.auth@1" },
      "$model": {
        "accounts": { "$key": "id", "id": "text", "name": "text" },
        "$auth": { "token": {
          "$credential": "text",
          "$verify": "$credential",
          "$actor": "authns.resolve($credential)"
        } }
      }
    }"#;
    assert!(
        load(def, auth_registry()).is_err(),
        "FINDING (HIGH): §16.5/§11.3 makes `$actor` a database-evaluated position, so an \
         app-registered `authns.resolve` call there must be a LOAD-TIME error, but it loaded",
    );
}

/// §16.5: an app-registered call in `$session` is a load error.
#[test]
fn app_fn_in_dollar_session_must_be_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.s@1.0.0",
      "$requires": { "authns": "test.auth@1" },
      "$model": {
        "accounts": { "$key": "id", "id": "text", "name": "text" },
        "sessions": { "$key": "id", "id": "text" },
        "$auth": { "token": {
          "$credential": "text",
          "$verify": "$credential",
          "$session": "authns.resolve($credential)",
          "$actor": "/accounts[$proof.account]"
        } }
      }
    }"#;
    assert!(
        load(def, auth_registry()).is_err(),
        "FINDING (HIGH): §16.5/§11.3 makes `$session` a database-evaluated position, so an \
         app-registered `authns.resolve` call there must be a LOAD-TIME error, but it loaded",
    );
}

/// §16.5: an app-registered call in an authenticator `$check` is a load error.
#[test]
fn app_fn_in_auth_check_must_be_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.ck@1.0.0",
      "$requires": { "authns": "test.auth@1" },
      "$model": {
        "accounts": { "$key": "id", "id": "text", "name": "text" },
        "$auth": { "token": {
          "$credential": "text",
          "$verify": "$credential",
          "$actor": "/accounts[$proof.account]",
          "$check": "authns.resolve($credential) == authns.resolve($credential)"
        } }
      }
    }"#;
    assert!(
        load(def, auth_registry()).is_err(),
        "FINDING (HIGH): §16.5/§11.3 makes an authenticator `$check` a database-evaluated \
         position, so an app-registered `authns.resolve` call there must be a LOAD-TIME error, \
         but it loaded",
    );
}

// ---------------------------------------------------------------------------
// CONTROLS (expected to PASS — the positions that ARE wired)
// ---------------------------------------------------------------------------

/// CONTROL: the SAME pure app call in a `$view` filter IS rejected at load with
/// the §16.5 diagnostic — proving the checker works and the auth positions are a
/// purely positional wiring hole, not a global failure of the origin rule.
#[test]
fn control_app_pure_fn_in_view_is_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.f@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "nums": { "$key": "id", "id": "text", "n": "int" },
        "big": { "$view": ".nums[:x | util.double(x.n) > 10] { id }" }
      }
    }"#;
    match load(def, util_registry()) {
        Err(err) => assert!(
            format!("{err:?}").contains("16.5"),
            "the view-filter rejection must cite §16.5, got: {err:?}",
        ),
        Ok(_) => panic!("a view-filter app call must be rejected at load"),
    }
}

/// CONTROL (mandate #6 — mutation-body legality): the SAME pure app call INSIDE a
/// mutation program body LOADS — the one legal position for an app-registered
/// namespace call (§16.5). Confirms the origin rule did not over-reach.
#[test]
fn control_app_fn_in_mutation_body_loads() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.m@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "rows": { "$key": "id", "id": "text", "n": "int = 0" },
        "$mut": { "add({ id: text, x: int })": [
          "doubled = util.double(@x)",
          "row = .rows + { id: @id, n: doubled }",
          "return row { id, n }"
        ] }
      }
    }"#;
    assert!(
        load(def, util_registry()).is_ok(),
        "a mutation body is the one legal position for an app-registered namespace call (§16.5); \
         it must load",
    );
}
