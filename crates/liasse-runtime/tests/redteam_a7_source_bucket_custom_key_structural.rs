#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! FINDING (§14.6): a source-backed bucket's custom `$key` using structural
//! bindings is rejected at load, though §14.6 explicitly permits it.
//!
//! §14.6 (verbatim): "A custom key MAY use declared output fields and structural
//! bindings:
//!
//! ```hjson
//! "$key": ["$source.external_id", "$from"]
//! ```
//!
//! The custom key MUST be unique for every generated row."
//!
//! So a source-backed bucket MAY declare a custom `$key` whose components are the
//! structural bindings `$source.<field>` / `$from` (or its declared output fields),
//! not only plain stored fields. The runtime is built for exactly this — its
//! `source_bucket.rs::compile_key` compiles each `$key` component as an expression
//! over the source scope, where `$source`/`$from`/`$until`/`$index` are bound — and
//! lib.rs lists §14.4–§14.6 as implemented with only the tzdb and future-window
//! seams, NOT custom keys.
//!
//! Yet the model's static key validation
//! (`liasse-model/src/build/keys.rs::key_field_type`) checks every `$key` component
//! against `shape.member(name)` — the collection's declared writable fields —
//! rejecting a structural-binding component as M-KEY "not a declared field of the
//! collection". So the §14.6 custom-key example cannot be loaded at all.
//!
//! CONTROL: the inferred-identity form (no `$key`) of the same package loads,
//! isolating the custom `$key` as the sole cause of the rejection.
//!
//! Root cause: `liasse-model/src/build/keys.rs::key_field_type` (~L98-101) —
//! `$key` components are validated only against declared collection fields, with no
//! exemption for a source-backed bucket's structural bindings / output fields.

mod support;

use liasse_runtime::{Engine, FixedGenerators, Precision};
use liasse_store::MemoryStore;
use support::store;

/// Genesis micros (2026-01-01T00:00:00Z) for the fixed clock.
const T0_MICROS: i128 = 1_767_225_600_000_000;

/// A source-backed bucket package. `key_clause` is spliced into the `$bucket`
/// collection: either empty (inferred identity, §14.6) or a custom `$key` line.
/// One interval per subscription (no `$repeat`); the sole subscription makes any
/// unique custom key trivially unique.
fn package(key_clause: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1,
  "$app": "t.b146.structkey@1.0.0",
  "$semantics": {{ "timestamp_precision": "s" }},
  "$model": {{
    "plans": {{ "$key": "id", "id": "text", "credits": "decimal" }},
    "subscriptions": {{
      "$key": "id",
      "id": "text",
      "plan": {{ "$ref": "/plans" }},
      "starts_at": "timestamp",
      "ends_at": "timestamp? = none"
    }},
    "credit_periods": {{
      "$bucket": {{
        "$source": ".subscriptions",
        "$from": "$source.starts_at",
        "$until": "$source.ends_at"
      }},{key_clause}
      "credits": "= /plans[$source.plan].credits"
    }}
  }},
  "$data": {{
    "plans": {{ "p": {{ "credits": "100" }} }},
    "subscriptions": {{ "s1": {{ "plan": "p", "starts_at": "1767225600" }} }}
  }}
}}"#
    )
}

fn load(instance: &str, definition: &str) -> Result<Engine<MemoryStore>, liasse_runtime::EngineError> {
    let mut generators = FixedGenerators::new(T0_MICROS, Precision::Micros);
    Engine::load(store(instance), definition, &mut generators)
}

/// CONTROL — the inferred-identity form (§14.6 "A source-backed collection MAY omit
/// `$key`") loads. This proves the package is otherwise well-formed, so the finding
/// below is attributable solely to the custom `$key`.
#[test]
fn inferred_identity_source_bucket_loads() {
    assert!(
        load("b146-inferred", &package("")).is_ok(),
        "a source-backed bucket with inferred identity (no $key) must load (§14.6)",
    );
}

/// FINDING — a source-backed bucket whose custom `$key` uses the structural
/// bindings `$source.id` and `$from` (the §14.6 example shape) MUST be accepted at
/// load: §14.6 explicitly permits a custom key built from structural bindings, and
/// the single subscription makes this key unique. The runtime rejects it at static
/// validation (M-KEY: `$from` / `$source.id` "not a declared field of the
/// collection"), so a §14.6-valid model cannot load. This test FAILS against that
/// behavior (the load errors instead of succeeding).
#[test]
fn source_bucket_custom_key_with_structural_bindings_must_load() {
    let definition = package(r#" "$key": ["$source.id", "$from"],"#);
    let result = load("b146-structkey", &definition);
    assert!(
        result.is_ok(),
        "§14.6 permits a source-backed bucket custom key built from structural bindings \
         (`[\"$source.external_id\", \"$from\"]`); the runtime compiles such keys \
         (source_bucket.rs::compile_key) but the model validator rejects them at load. \
         Expected the package to load; got {:?}",
        result.err(),
    );
}
