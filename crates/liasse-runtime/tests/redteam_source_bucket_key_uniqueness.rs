#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! FINDING (§14.6): a source-backed bucket's custom `$key` that COLLIDES across
//! generated rows is neither rejected at admission nor deduplicated — the runtime
//! materializes two generated rows sharing one custom key, though §14.6 states
//! the custom key MUST be unique for every generated row.
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
//! The model records this as an admission-time invariant — `liasse-model`'s
//! `bucket.rs::check_custom_key` says: "Uniqueness 'for every generated row' is a
//! runtime property of the derived series and is enforced at admission." But NO
//! code enforces it: `source_bucket.rs::validate` (the sole per-transition source
//! bucket admission check, wired through `eval.rs::validate_source_series`) only
//! checks recurrence-series validity (`recurring_intervals`), never key
//! uniqueness, and `materialize` pushes every generated row without deduplicating
//! by identity. Stored-collection `DuplicateKey` checks (`interp.rs`, `rules.rs`,
//! `seed.rs`) never run over derived bucket rows.
//!
//! Here two subscriptions in the same region drive a bucket keyed
//! `["$source.region"]`. Both generated period rows take the custom key `eu`, so
//! §14.6 requires the transition establishing this (the seed) be rejected — or,
//! failing that, the read to expose distinct generated rows. The current
//! implementation does neither: the package loads and `.periods.$all` returns two
//! rows both keyed `eu`.
//!
//! Root cause: `crates/liasse-runtime/src/source_bucket.rs::validate`
//! (~L410-424) performs no custom-key uniqueness check;
//! `crates/liasse-runtime/src/eval.rs::validate_source_series` (~L375-385) calls
//! only that series check.

mod support;

use liasse_runtime::{Engine, EngineError};
use liasse_store::MemoryStore;
use support::store;

/// 2026-01-01T00:00:00Z, in seconds (the package pins second precision).
const T0_SECONDS: &str = "1767225600";

/// A source-backed bucket package. `key_clause` is spliced in as the bucket
/// collection's `$key` line. Two subscriptions share region `eu`; `starts_at`
/// differs so the inferred/composite-with-id forms stay unique while a
/// region-only key collides.
fn package(app: &str, key_clause: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1,
  "$app": "{app}",
  "$semantics": {{ "timestamp_precision": "s" }},
  "$model": {{
    "subscriptions": {{
      "$key": "id",
      "id": "text",
      "region": "text",
      "starts_at": "timestamp",
      "ends_at": "timestamp? = none"
    }},
    "periods": {{
      "$bucket": {{
        "$source": ".subscriptions",
        "$from": "$source.starts_at",
        "$until": "$source.ends_at"
      }},
      {key_clause}
      "region": "= $source.region"
    }},
    "all_periods": {{ "$view": ".periods.$all {{ region: $source.region, sid: $source.id }}" }}
  }},
  "$data": {{
    "subscriptions": {{
      "s1": {{ "region": "eu", "starts_at": "{T0_SECONDS}" }},
      "s2": {{ "region": "eu", "starts_at": "1767312000" }}
    }}
  }}
}}"#
    )
}

fn load(instance: &str, definition: &str) -> Result<Engine<MemoryStore>, EngineError> {
    let mut generators = support::generator();
    Engine::load(store(instance), definition, &mut generators)
}

/// The `region`(=custom key) value of every row `.periods.$all` returns.
fn generated_keys(engine: &Engine<MemoryStore>) -> Vec<String> {
    let view = engine
        .view_at_head("all_periods")
        .expect("view ok")
        .expect("view present");
    view.rows()
        .iter()
        .map(|r| match r.field("region").expect("region cell") {
            liasse_runtime::Value::Text(t) => t.as_str().to_owned(),
            other => panic!("region not text: {other:?}"),
        })
        .collect()
}

/// CONTROL — a composite custom key `["$source.region", "$source.id"]` (the
/// §14.6 example shape, distinct per source row) loads and generates two rows
/// with DISTINCT custom keys. This proves the package, the source-bucket custom
/// key, and the `.periods.$all` read are all sound; the finding differs only in
/// dropping the disambiguating `$source.id` component.
#[test]
fn unique_composite_custom_key_generates_distinct_rows() {
    let engine = load(
        "b146-uniq-control",
        &package("t.b146.uniqctl@1.0.0", r#""$key": ["$source.region", "$source.id"],"#),
    )
    .expect("the unique-key form must load (§14.6)");
    let keys = generated_keys(&engine);
    assert_eq!(keys.len(), 2, "one generated period per subscription");
}

/// FINDING — a region-only custom key `["$source.region"]` makes both generated
/// period rows take the same custom key `eu`. §14.6 requires the custom key be
/// unique for every generated row, so the establishing transition MUST reject —
/// or, at minimum, no committed read may expose two generated rows sharing a
/// custom key.
///
/// This test FAILS against the current implementation: the seed is admitted
/// (`validate_source_series` checks only series validity) and `.periods.$all`
/// returns two rows both keyed `eu`, so neither the rejection nor the uniqueness
/// invariant holds.
#[test]
fn colliding_custom_key_must_reject_or_stay_unique() {
    let result = load(
        "b146-uniq-finding",
        &package("t.b146.uniqbug@1.0.0", r#""$key": ["$source.region"],"#),
    );
    match result {
        // §14.6 enforced at admission: the non-unique series is rejected. Correct.
        Err(_) => {}
        Ok(engine) => {
            let keys = generated_keys(&engine);
            let mut unique = keys.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(
                unique.len(),
                keys.len(),
                "§14.6: the custom key MUST be unique for every generated row, but \
                 `.periods.$all` returned generated rows with duplicate custom keys {keys:?} \
                 and admission did not reject the establishing transition",
            );
        }
    }
}
