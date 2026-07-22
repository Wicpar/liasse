#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
#![allow(clippy::doc_lazy_continuation)]
//! RED-TEAM (WAVE 4) — §14.6: the wave-3 unbounded-recurring uniqueness fix
//! OVER-REJECTS phase-offset recurring grids whose `$from` values never coincide.
//!
//! The wave-3 cross-source soundness probe (`source_bucket.rs::probe_identity`)
//! evaluates each unbounded source's custom key under a COMMON synthetic
//! `$from = now`, so it can only tell whether the key distinguishes sources
//! INDEPENDENT of the period. That is sound for detecting keys that never vary by
//! `$from`, but it also fires when two sources share every non-`$from` component
//! yet their grids are DISJOINT — the common `$from` hides the phase offset.
//!
//! §14.6 (verbatim): "The custom key MUST be unique for every generated row." Two
//! weekly (`$repeat P7D`, unbounded) sources in the same region `eu`, phase-offset
//! by ONE day, keyed `["$source.region", "$from"]`, generate grids
//!   s1: { T0, T0+7d, T0+14d, ... }
//!   s2: { T0+1d, T0+8d, T0+15d, ... }
//! which are DISJOINT — `(φ1 − φ2) = 1d` is NOT a multiple of `gcd(P7D, P7D) = 7d`
//! — so EVERY generated key `(eu, $from)` is distinct across the whole series.
//! §14.6 is satisfied and the seed MUST load. The wave-3 probe rejects it as
//! `DuplicateKey`, a false rejection.
//!
//! All values follow from §14.5/§14.6 and the fixed T0 weekly-grid arithmetic
//! alone (two anchors one day apart), never from observed behaviour.
//!
//! Root cause: `crates/liasse-runtime/src/source_bucket.rs::probe_identity`
//! (~L507) holds `$from` constant, so it cannot distinguish provably-disjoint
//! grids (a phase offset that is not a multiple of the period) from provably-
//! aligned ones (an offset that is).

mod support;

use liasse_runtime::{
    CallOutcome, CallRequest, Engine, EngineError, FixedGenerators, Precision, Timestamp, Value,
};
use liasse_store::MemoryStore;
use support::store;

/// 2026-01-01T00:00:00Z in microseconds — genesis T0 and s1's series start.
const T0: i128 = 1_767_225_600_000_000;
/// One day in microseconds.
const DAY: i128 = 86_400_000_000;
/// s2's series start: ONE day after T0, so its weekly grid never coincides with
/// s1's (offset 1d is not a multiple of the 7-day period).
const S2_START: i128 = T0 + DAY;

/// A weekly recurring source-backed bucket over two same-region subscriptions,
/// phase-offset by one day. `key_clause` is spliced in as the bucket `$key`.
fn definition(app: &str, key_clause: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1, "$app": "{app}",
  "$model": {{
    "plans": {{ "$key": "id", "id": "text", "credits": "decimal", "period": "period?" }},
    "subscriptions": {{
      "$key": "id", "id": "text", "region": "text",
      "plan": {{ "$ref": "/plans" }},
      "starts_at": "timestamp", "ends_at": "timestamp? = none"
    }},
    "grids": {{
      "$bucket": {{
        "$source": ".subscriptions",
        "$from": "$source.starts_at",
        "$until": "$source.ends_at",
        "$repeat": "/plans[$source.plan].period"
      }},
      {key_clause}
      "region": "= $source.region"
    }},
    "$mut": {{
      "window({{ a: timestamp, b: timestamp }})":
        "return .grids.$between(@a, @b) {{ region: $source.region, from: $from, sid: $source.id }}"
    }}
  }},
  "$data": {{
    "plans": {{ "p": {{ "credits": "100", "period": "P7D" }} }},
    "subscriptions": {{
      "s1": {{ "region": "eu", "plan": "p", "starts_at": "{T0}" }},
      "s2": {{ "region": "eu", "plan": "p", "starts_at": "{S2_START}" }}
    }}
  }}
}}"#
    )
}

fn load(instance: &str, definition: &str) -> Result<Engine<MemoryStore>, EngineError> {
    let mut generators = FixedGenerators::new(T0, Precision::Micros);
    Engine::load(store(instance), definition, &mut generators)
}

fn at(micros: i128) -> Value {
    Value::Timestamp(Timestamp::new(micros, Precision::Micros))
}

/// Read `.grids.$between(a, b)` and return each generated row's `(region, from)` —
/// the full custom-key tuple.
fn window_keys(engine: &mut Engine<MemoryStore>, a: i128, b: i128) -> Vec<(String, String)> {
    let mut generators = FixedGenerators::new(T0, Precision::Micros);
    let request = CallRequest::new("window").arg("a", at(a)).arg("b", at(b));
    let outcome = engine.call(&request, &mut generators).expect("bounded temporal read resolves");
    assert!(
        !matches!(outcome, CallOutcome::Rejected(_)),
        "a valid bounded temporal read is not rejected, got {outcome:?}",
    );
    let response = outcome.response().expect("the read-only mutation returns a value");
    match response.to_wire() {
        serde_json::Value::Array(rows) => rows
            .into_iter()
            .map(|row| {
                let region =
                    row["region"].as_str().unwrap_or_else(|| panic!("`region` string, got {row}"));
                let from = row["from"].as_str().unwrap_or_else(|| panic!("`from` string, got {row}"));
                (region.to_owned(), from.to_owned())
            })
            .collect(),
        other => panic!("expected an array of derived rows, got {other}"),
    }
}

// ── THE FINDING ──────────────────────────────────────────────────────────────
// Key `["$source.region", "$from"]`, two same-region weekly sources phase-offset
// by one day: the grids are DISJOINT, so every `(eu, $from)` key is unique
// (§14.6). The seed MUST load; the wave-3 probe over-rejects it.
#[test]
fn phase_offset_disjoint_grids_must_load() {
    let definition =
        definition("t.w4b146.disjoint@1.0.0", r#""$key": ["$source.region", "$from"],"#);
    let mut engine = load("w4-b146-disjoint", &definition).expect(
        "§14.6: phase-offset weekly grids (offset 1d, period 7d) are disjoint, so every custom \
         key is unique — the seed must load",
    );
    // A window spanning several weeks of BOTH sources: because the grids never
    // coincide, every `(region, $from)` key the window returns is distinct.
    let rows = window_keys(&mut engine, T0, T0 + 21 * DAY);
    assert!(rows.len() >= 4, "the window spans several periods of both sources, got {rows:?}");
    let mut distinct = rows.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        rows.len(),
        "§14.6: every generated custom key `(region, $from)` is distinct across the disjoint \
         grids, got {rows:?}",
    );
}

// ── CONTROL 1: the disambiguating `["$source.id", "$from"]` key ────────────────
// The source id distinguishes each generated row, so the key is unique regardless
// of grid alignment; this already loads and isolates the finding to the phase
// analysis (dropping `$source.id` while the grids stay disjoint must STILL load).
#[test]
fn source_id_key_loads() {
    let definition = definition("t.w4b146.sidctl@1.0.0", r#""$key": ["$source.id", "$from"],"#);
    let mut engine = load("w4-b146-sid", &definition).expect("a source-distinguishing key loads");
    let rows = window_keys(&mut engine, T0, T0 + 21 * DAY);
    assert!(!rows.is_empty(), "the window returns generated rows, got {rows:?}");
}
