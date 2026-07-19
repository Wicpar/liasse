#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM — Phase-7b mandate-7 (§16.5) CONVERGENCE evidence: the positions and
//! rules that are correctly wired. Every test here is expected to PASS.
//!
//! These pin the properties the mandate asks to protect against regression:
//!
//! * #4 EFFECT-BEFORE-ORIGIN order: in a database-evaluated position, a
//!   generated/verifier app fn reports the §16.3 EFFECT diagnostic (the stronger,
//!   position-wide violation), NOT the §16.5 origin one. `check_host_call` runs
//!   the effect check first (`liasse-expr/src/check/views.rs:308`).
//!
//! * #6 MUTATION-BODY LEGALITY (the converse of the origin rule): an
//!   app-registered fn of ANY effect class (pure/verifier/generated) inside a
//!   mutation program body LOADS — the origin rule does not over-reach into the
//!   one legal position.
//!
//! * #3 NO OVER-BROAD REJECTION: a built-in `string.*` call and the language
//!   generators `uuid()`/`now()` still load in every database-evaluated position
//!   — they never reach the §16.5 origin check (`string.*` resolves via
//!   `core_string_fn` before `namespace_op`; `uuid`/`now` are language calls).

use liasse_host::sim::{Behavior, SimNamespace};
use liasse_ident::InstanceId;
use liasse_runtime::{
    ContractName, EffectClass, Engine, EngineError, FixedGenerators, InterfaceHash, OpSignature,
    Precision, Registry, Version,
};
use liasse_store::MemoryStore;
use liasse_value::Type;

/// `test.util@1`: a pure `double`, a generated `mint`, and a verifier `chk`.
fn reg() -> Registry {
    let ns = SimNamespace::builder(
        ContractName::parse("test.util").expect("contract name"),
        Version::new(1, 2, 0),
        InterfaceHash::new("ih-util-1"),
    )
    .function("double", OpSignature::new([Type::Int], Type::Int), EffectClass::Pure, Behavior::Double)
    .function("mint", OpSignature::new([Type::Int], Type::Int), EffectClass::Generated, Behavior::Token)
    .function("chk", OpSignature::new([Type::Text], Type::Json), EffectClass::Verifier, Behavior::Accept)
    .build();
    let mut r = Registry::new();
    r.register_namespace(Box::new(ns));
    r
}

fn load(def: &str) -> Result<Engine<MemoryStore>, EngineError> {
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = FixedGenerators::new(1_700_000_000_000_000, Precision::Micros);
    Engine::load_with_hosts(store, def, &mut g, reg())
}

fn reject_detail(def: &str) -> String {
    match load(def) {
        Ok(_) => panic!("expected a load-time rejection, but the package loaded"),
        Err(err) => format!("{err:?}"),
    }
}

/// #4: a GENERATED app fn in a view reports the §16.3 effect rule, not §16.5 —
/// the effect check precedes the origin check and the order is not inverted.
#[test]
fn generated_fn_in_view_reports_effect_rule_not_origin() {
    let detail = reject_detail(
        r#"{ "$liasse":1,"$app":"t@1.0.0","$requires":{"util":"test.util@1"},
             "$model":{"rows":{"$key":"id","id":"text","n":"int"},
               "v":{"$view":".rows[:x | util.mint(x.n) > 0] { id }"}}}"#,
    );
    assert!(detail.contains("16.3") && detail.contains("generated"), "expected §16.3 effect dx: {detail}");
    assert!(!detail.contains("16.5"), "the §16.3 effect rule must win over §16.5 origin: {detail}");
}

/// #4: a VERIFIER app fn in a view likewise reports the §16.3 effect rule.
#[test]
fn verifier_fn_in_view_reports_effect_rule_not_origin() {
    let detail = reject_detail(
        r#"{ "$liasse":1,"$app":"t@1.0.0","$requires":{"util":"test.util@1"},
             "$model":{"rows":{"$key":"id","id":"text","c":"text"},
               "v":{"$view":".rows[:x | util.chk(x.c) == util.chk(x.c)] { id }"}}}"#,
    );
    assert!(detail.contains("16.3") && detail.contains("verifier"), "expected §16.3 effect dx: {detail}");
    assert!(!detail.contains("16.5"), "the §16.3 effect rule must win over §16.5 origin: {detail}");
}

/// #6: a VERIFIER app fn inside a mutation body loads (any effect class legal).
#[test]
fn verifier_fn_in_mutation_body_loads() {
    assert!(
        load(
            r#"{ "$liasse":1,"$app":"t@1.0.0","$requires":{"util":"test.util@1"},
                 "$model":{"rows":{"$key":"id","id":"text","p":"json"},
                   "$mut":{"add({ id: text, cred: text })":[
                     "proof = util.chk(@cred)","row = .rows + { id: @id, p: proof }","return row { id }"]}}}"#,
        )
        .is_ok(),
        "a verifier app fn is legal in a mutation body (§16.5)",
    );
}

/// #6: a GENERATED app fn inside a mutation body loads.
#[test]
fn generated_fn_in_mutation_body_loads() {
    assert!(
        load(
            r#"{ "$liasse":1,"$app":"t@1.0.0","$requires":{"util":"test.util@1"},
                 "$model":{"rows":{"$key":"id","id":"text","n":"int = 0"},
                   "$mut":{"add({ id: text, x: int })":[
                     "m = util.mint(@x)","row = .rows + { id: @id, n: m }","return row { id, n }"]}}}"#,
        )
        .is_ok(),
        "a generated app fn is legal in a mutation body (§16.5)",
    );
}

/// #3: built-in `string.*` in a view/`$normalize` and the language generators
/// `uuid()`/`now()` in defaults still load — no spurious §16.5 rejection.
#[test]
fn builtins_and_language_generators_still_load_in_db_read_positions() {
    assert!(
        load(
            r#"{ "$liasse":1,"$app":"t@1.0.0",
                 "$model":{"rows":{"$key":"id","id":"uuid = uuid()",
                     "email":{"$type":"text","$normalize":"string.lower(string.trim(.))"},
                     "at":"timestamp = now()"},
                   "lows":{"$view":".rows[:x | string.lower(x.email) == x.email] { id }"}}}"#,
        )
        .is_ok(),
        "built-in `string.*` and language generators `uuid()`/`now()` must not be caught by the \
         §16.5 origin check",
    );
}
