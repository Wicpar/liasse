#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM — Phase-7b mandate-7 (§16.5) adversarial suite: the role/surface
//! read positions.
//!
//! §16.5 lists a `$view` (its source selection, filter, projection, `$sort`) as a
//! DATABASE-EVALUATED position; §10.3 role `$members` is a member-selecting view.
//! An app-registered namespace call in either is a LOAD-TIME error.
//!
//! FINDINGS (HIGH/MEDIUM — wiring holes):
//!
//! * ROLE `$members` (§10.3): host-checked by NOBODY. The model surface phase
//!   matches `$members` with an empty arm
//!   (`crates/liasse-model/src/surface.rs:134` — `"$auth"|"$members"|"$recursive"
//!   => {}`), and the runtime `compile_surface_views` skips every `$`-member
//!   (`crates/liasse-runtime/src/compiled.rs:1774-1776`). An app-registered call
//!   in `$members` is admitted at load.
//!
//! * ROLE-scoped `$view` (§10.1/§10.3): the model `check_view(public=false)` does
//!   NOT fully type it (`surface.rs` — "only the public path is additionally
//!   fully typed"), and the runtime `compile_one_surface_view` SWALLOWS the
//!   §16.5 compile error with `let Ok(...) else { return; }`
//!   (`crates/liasse-runtime/src/compiled.rs:~1862`), dropping the surface
//!   instead of failing the load. So §16.5's mandated LOAD-TIME error is never
//!   raised — the package loads with the offending role view silently unserved.
//!
//! Each FINDING test asserts the SPEC-CORRECT outcome (load rejected) and so
//! FAILS against the current build. The CONTROLS assert the wired positions and
//! PASS: a `$public` surface view and a top-level computed value both reject the
//! same app call, isolating the defect to the role positions.

use liasse_host::sim::{Behavior, SimNamespace};
use liasse_ident::InstanceId;
use liasse_runtime::{
    ContractName, EffectClass, Engine, EngineError, FixedGenerators, InterfaceHash, OpSignature,
    Precision, Registry, Version,
};
use liasse_store::MemoryStore;
use liasse_value::Type;

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

fn load(def: &str) -> Result<Engine<MemoryStore>, EngineError> {
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut g = FixedGenerators::new(1_700_000_000_000_000, Precision::Micros);
    Engine::load_with_hosts(store, def, &mut g, util_registry())
}

// ---------------------------------------------------------------------------
// FINDINGS (expected to FAIL until role positions are host-checked at load)
// ---------------------------------------------------------------------------

/// §16.5/§10.3: an app-registered call in a role `$members` view is a load error.
#[test]
fn app_fn_in_role_members_must_be_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.rm@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "accounts": { "$key": "id", "id": "text", "n": "int" },
        "$auth": { "tok": {
          "$credential": "text",
          "$verify": "$credential",
          "$actor": "/accounts[$proof.id]"
        } },
        "$roles": {
          "member": {
            "$auth": "tok",
            "$members": ".accounts[:a | util.double(a.n) > 0]"
          }
        }
      }
    }"#;
    assert!(
        load(def).is_err(),
        "FINDING (HIGH): §16.5/§10.3 makes a role `$members` view a database-evaluated position, \
         so an app-registered `util.double` call there must be a LOAD-TIME error, but the \
         package loaded — `$members` is host-checked by neither the model nor the runtime",
    );
}

/// §16.5/§10.1: an app-registered call in a role-scoped `$view` is a load error;
/// the runtime must not swallow the §16.5 rejection and silently drop the view.
#[test]
fn app_fn_in_role_view_must_be_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.rv@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "accounts": { "$key": "id", "id": "text", "n": "int" },
        "$auth": { "tok": {
          "$credential": "text",
          "$verify": "$credential",
          "$actor": "/accounts[$proof.id]"
        } },
        "$roles": {
          "member": {
            "$auth": "tok",
            "$members": "/accounts",
            "big": { "$view": ".accounts[:a | util.double(a.n) > 0] { id }" }
          }
        }
      }
    }"#;
    assert!(
        load(def).is_err(),
        "FINDING (HIGH): §16.5 makes a role-scoped `$view` a database-evaluated position, so an \
         app-registered `util.double` call there must be a LOAD-TIME error; instead the runtime \
         `compile_one_surface_view` swallows the §16.5 diagnostic (`let Ok(..) else return`) and \
         the package loads with the view silently dropped",
    );
}

// ---------------------------------------------------------------------------
// CONTROLS (expected to PASS — the wired positions)
// ---------------------------------------------------------------------------

/// CONTROL: the SAME app call in a `$public` surface view IS rejected at load
/// (the model fully types a public `$view`), isolating the defect to role views.
#[test]
fn control_app_fn_in_public_surface_view_is_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.pv@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "nums": { "$key": "id", "id": "text", "n": "int" },
        "$public": {
          "big": { "$view": ".nums[:x | util.double(x.n) > 10] { id }" }
        }
      }
    }"#;
    assert!(
        load(def).is_err(),
        "a `$public` surface view must reject an app-registered namespace call at load",
    );
}

/// CONTROL: the SAME app call in a computed value IS rejected with the §16.5
/// diagnostic — the origin rule fires correctly for the state-tree positions.
#[test]
fn control_app_fn_in_computed_is_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.c@1.0.0",
      "$requires": { "util": "test.util@1" },
      "$model": {
        "rows": { "$key": "id", "id": "text", "n": "int", "c": "= util.double(.n)" }
      }
    }"#;
    match load(def) {
        Err(err) => assert!(
            format!("{err:?}").contains("16.5"),
            "the computed-value rejection must cite §16.5, got: {err:?}",
        ),
        Ok(_) => panic!("a computed-value app call must be rejected at load"),
    }
}
