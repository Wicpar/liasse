#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM — Phase-7 mandate-7 (§16.5): the meter (§15) read positions.
//!
//! §16.5 lists "bucket, meter, and placement expressions (§14, §15, §18.4)" among
//! the DATABASE-EVALUATED positions. A `$requires`-registered namespace call in a
//! meter's `$sources` pool view, `$eligible`, `$order`, or a `$consumes`
//! amount/time/metadata expression is therefore a LOAD-TIME error (§16.5).
//!
//! FINDING: unlike a bucket bound (`compile_bucket`, which propagates the §16.5
//! error and fails the load) or a surface `$view`/role `$members` (which audit the
//! position loudly before the tolerant compile), the meter compiler
//! (`crates/liasse-runtime/src/meter`) swallows a compile error with
//! `if let Ok(compiled) = compile_meter(...)` / `if let Ok(spend) =
//! compile_consumes(...)`, silently DROPPING the meter and LOADING the package.
//! Its scopes also carry no host signatures, so the offending app call cannot even
//! be recognised as a §16.5 violation. §16.5's mandated load-time error is never
//! raised.
//!
//! Each FINDING asserts the SPEC-CORRECT outcome (load rejected). The CONTROL — the
//! same app call in a bucket bound, an already-wired database-evaluated position —
//! PASSES, isolating the defect to the meter positions.

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
// FINDINGS (expected to FAIL until meter positions are host-checked at load)
// ---------------------------------------------------------------------------

/// §16.5/§15.2: an app-registered call in a meter `$eligible` is a load error.
#[test]
fn app_fn_in_meter_eligible_must_be_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.me@1.0.0",
      "$semantics": { "timestamp_precision": "s" },
      "$requires": { "util": "test.util@1" },
      "$model": {
        "users": {
          "$key": "id",
          "id": "text",
          "topups": { "$key": "id", "id": "text", "amount": "decimal", "n": "int" },
          "spends": {
            "$key": "id",
            "$consumes": "credits",
            "id": "uuid = uuid()",
            "amount": "decimal",
            "occurred_at": "timestamp = now()"
          },
          "$limits": {
            "credits": {
              "$sources": { "topup": ".topups { $quantity: .amount, n }" },
              "$eligible": "util.double(pool.n) > 0"
            }
          },
          "$mut": {
            "consume": [
              "spend = .spends + { amount: @amount }",
              "return spend { id }"
            ]
          }
        },
        "$public": { "wallet": { "$mut": { "consume": ".users[@user].consume" } } }
      }
    }"#;
    match load(def) {
        Err(err) => assert!(
            format!("{err:?}").contains("16.5"),
            "the meter `$eligible` rejection must cite §16.5, got: {err:?}",
        ),
        Ok(_) => panic!(
            "FINDING (§16.5/§15.2): a meter `$eligible` is a database-evaluated position, so an \
             app-registered `util.double` call there must be a LOAD-TIME error; instead the meter \
             compiler swallows the error and the package loads with the meter silently dropped",
        ),
    }
}

/// §16.5/§15.1: an app-registered call in a meter `$sources` pool view is a load
/// error.
#[test]
fn app_fn_in_meter_source_must_be_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.ms@1.0.0",
      "$semantics": { "timestamp_precision": "s" },
      "$requires": { "util": "test.util@1" },
      "$model": {
        "users": {
          "$key": "id",
          "id": "text",
          "topups": { "$key": "id", "id": "text", "amount": "decimal", "n": "int" },
          "spends": {
            "$key": "id",
            "$consumes": "credits",
            "id": "uuid = uuid()",
            "amount": "decimal",
            "occurred_at": "timestamp = now()"
          },
          "$limits": {
            "credits": {
              "$sources": { "topup": ".topups[:t | util.double(t.n) > 0] { $quantity: .amount }" }
            }
          },
          "$mut": {
            "consume": [
              "spend = .spends + { amount: @amount }",
              "return spend { id }"
            ]
          }
        },
        "$public": { "wallet": { "$mut": { "consume": ".users[@user].consume" } } }
      }
    }"#;
    assert!(
        load(def).is_err(),
        "FINDING (§16.5/§15.1): a meter `$sources` pool view is a database-evaluated position, so \
         an app-registered `util.double` call there must be a LOAD-TIME error",
    );
}

/// §16.5/§15.1: an app-registered call in a `$consumes` amount expression is a
/// load error.
#[test]
fn app_fn_in_consumes_amount_must_be_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.ca@1.0.0",
      "$semantics": { "timestamp_precision": "s" },
      "$requires": { "util": "test.util@1" },
      "$model": {
        "users": {
          "$key": "id",
          "id": "text",
          "topups": { "$key": "id", "id": "text", "amount": "decimal" },
          "spends": {
            "$key": "id",
            "$consumes": { "credits": { "$amount": "util.double(.units)" } },
            "id": "uuid = uuid()",
            "units": "int",
            "occurred_at": "timestamp = now()"
          },
          "$limits": {
            "credits": {
              "$sources": { "topup": ".topups { $quantity: .amount }" }
            }
          },
          "$mut": {
            "consume": [
              "spend = .spends + { units: @units }",
              "return spend { id }"
            ]
          }
        },
        "$public": { "wallet": { "$mut": { "consume": ".users[@user].consume" } } }
      }
    }"#;
    assert!(
        load(def).is_err(),
        "FINDING (§16.5/§15.1): a `$consumes` amount is a database-evaluated position, so an \
         app-registered `util.double` call there must be a LOAD-TIME error",
    );
}

// ---------------------------------------------------------------------------
// CONTROL (expected to PASS — an already-wired database-evaluated position)
// ---------------------------------------------------------------------------

/// CONTROL: the SAME app call in a bucket bound IS rejected at load (the bucket
/// compiler propagates the §16.5 error), isolating the defect to meter positions.
#[test]
fn control_app_fn_in_bucket_bound_is_rejected_at_load() {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.bb@1.0.0",
      "$semantics": { "timestamp_precision": "s" },
      "$requires": { "util": "test.util@1" },
      "$model": {
        "sessions": {
          "$key": "id",
          "id": "text",
          "n": "int",
          "$bucket": { "$until": "util.double(.n)" }
        }
      }
    }"#;
    assert!(
        load(def).is_err(),
        "CONTROL: a bucket bound must reject an app-registered namespace call at load",
    );
}
