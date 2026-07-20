#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
#![allow(clippy::doc_lazy_continuation)]
//! RED-TEAM (WAVE 3) — §14.6 custom-key uniqueness is checked over a TRUNCATED
//! 2-period horizon, so a genuine collision at period 3+ (across sources whose
//! recurrence grids align) is admitted and later materialized.
//!
//! The wave-2 fix (commit 0d9427f, `source_bucket.rs::validate`) enforces §14.6
//! custom-key uniqueness by enumerating each source row's generated rows into a
//! shared `seen` set — but for an UNBOUNDED recurring series it enumerates only two
//! periods deep (`uniqueness_horizon` = `period.advance_from(from, 2)`), on the
//! theory that "a custom key that fails to vary per period repeats within the first
//! two periods" and that "a cross-period-varying key that still collides only across
//! sources at a wide offset ... an unbounded series is only ever read through a
//! bounded selector (§14.5), so no unbounded materialization exposes it."
//!
//! Both defenses fail. §14.6 (verbatim): "A custom key MAY use declared output
//! fields and structural bindings: `\"$key\": [\"$source.external_id\", \"$from\"]`.
//! The custom key MUST be unique for **every generated row**." The MUST is over the
//! whole series, not a 2-period prefix; and a bounded `.$between` selector DOES
//! expose the collision.
//!
//! THE COLLISION. Two subscriptions in one weekly (`P7D`) recurring bucket keyed
//! `["$from"]` (the period start — a legitimate structural-binding key):
//!   * s1 starts at T0            -> periods start T0, T0+7d, T0+14d, T0+21d, ...
//!   * s2 starts at T0 + 2 weeks  -> periods start        T0+14d, T0+21d, ...
//! s1's period index 2 (`$from` = T0+14d) and s2's period index 0 (`$from` = T0+14d)
//! are two DISTINCT generated rows resolving the SAME custom key T0+14d. §14.6 is
//! violated and the establishing seed MUST be rejected.
//!
//! It is missed because the uniqueness pass enumerates each source only to its own
//! 2-period horizon: s1 -> starts {T0, T0+7d} (index 2 at T0+14d is NOT reached),
//! s2 -> starts {T0+14d, T0+21d}. The shared `seen` set is {T0, T0+7d, T0+14d,
//! T0+21d} — all distinct — so the seed is admitted. Then a bounded read
//! `.$between(T0+14d, T0+21d)` generates s1's index 2 and s2's index 0 and returns
//! BOTH rows keyed T0+14d, exposing the collision the §14.6 MUST forbids.
//!
//! All values follow from §14.1/§14.5/§14.6 and the fixed T0 clock arithmetic
//! alone (weekly grid over two anchors), never from observed behaviour.
//!
//! Root cause: `crates/liasse-runtime/src/source_bucket.rs::uniqueness_horizon`
//! (~L485) caps enumeration of an unbounded recurring series at 2 periods, so a
//! collision first appearing at period 3+ (here s1's index 2 against s2's index 0)
//! is neither rejected at admission nor deduplicated.

mod support;

use std::collections::BTreeSet;

use liasse_runtime::{
    CallOutcome, CallRequest, Engine, EngineError, FixedGenerators, Precision, Timestamp, Value,
};
use liasse_store::MemoryStore;
use support::store;

/// 2026-01-01T00:00:00Z in microseconds — genesis T0, the fixed engine clock and
/// s1's series start.
const T0: i128 = 1_767_225_600_000_000;
/// One day in microseconds.
const DAY: i128 = 86_400_000_000;
/// One week in microseconds (the `P7D` recurrence period).
const WEEK: i128 = 7 * DAY;
/// s2's series start: two full weeks after T0, so s2's period 0 shares a boundary
/// with s1's period 2.
const S2_START: i128 = T0 + 2 * WEEK;

/// A weekly recurring source-backed bucket over two subscriptions. `key_clause` is
/// spliced in as the bucket collection's `$key`. s1 starts at T0, s2 two weeks
/// later, both unbounded (`ends_at = none`), so their weekly grids coincide from
/// T0+14d onward.
fn definition(app: &str, key_clause: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1, "$app": "{app}",
  "$model": {{
    "plans": {{ "$key": "id", "id": "text", "credits": "decimal", "period": "period?" }},
    "subscriptions": {{
      "$key": "id", "id": "text",
      "plan": {{ "$ref": "/plans" }},
      "starts_at": "timestamp", "ends_at": "timestamp? = none"
    }},
    "credit_periods": {{
      "$bucket": {{
        "$source": ".subscriptions",
        "$from": "$source.starts_at",
        "$until": "$source.ends_at",
        "$repeat": "/plans[$source.plan].period"
      }},
      {key_clause}
      "credits": "= /plans[$source.plan].credits"
    }},
    "$mut": {{
      "collide({{ a: timestamp, b: timestamp }})":
        "return .credit_periods.$between(@a, @b) {{ from: $from, sid: $source.id }}"
    }}
  }},
  "$data": {{
    "plans": {{ "p": {{ "credits": "100", "period": "P7D" }} }},
    "subscriptions": {{
      "s1": {{ "plan": "p", "starts_at": "{T0}" }},
      "s2": {{ "plan": "p", "starts_at": "{S2_START}" }}
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

/// Read `.credit_periods.$between(a, b)` and return each generated row's
/// `(from, sid)` — its custom-key input and the source it came from.
fn collide_rows(
    engine: &mut Engine<MemoryStore>,
    a: i128,
    b: i128,
) -> Vec<(String, String)> {
    let mut generators = FixedGenerators::new(T0, Precision::Micros);
    let request = CallRequest::new("collide").arg("a", at(a)).arg("b", at(b));
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
                let from = row["from"].as_str().unwrap_or_else(|| panic!("`from` is a string, got {row}"));
                let sid = row["sid"].as_str().unwrap_or_else(|| panic!("`sid` is a string, got {row}"));
                (from.to_owned(), sid.to_owned())
            })
            .collect(),
        other => panic!("expected an array of derived rows, got {other}"),
    }
}

// ── THE FINDING ──────────────────────────────────────────────────────────────
// Key `["$from"]`: s1's period-2 row and s2's period-0 row both resolve custom key
// T0+14d, a §14.6 violation. Admission must reject the seed; failing that, no
// committed read may expose two generated rows sharing a custom key.
#[test]
fn colliding_key_beyond_two_period_horizon_must_reject_or_stay_unique() {
    let definition = definition("t.w3b146.horizoncollide@1.0.0", r#""$key": ["$from"],"#);
    match load("w3-b146-collide", &definition) {
        // §14.6 enforced at admission: the non-unique series is rejected. Correct.
        Err(_) => {}
        Ok(mut engine) => {
            // The window [T0+14d, T0+21d) covers exactly s1 index 2 and s2 index 0.
            let rows = collide_rows(&mut engine, T0 + 14 * DAY, T0 + 21 * DAY);
            assert_eq!(
                rows.len(),
                2,
                "the collision window returns both generated rows (s1 index 2, s2 index 0), got {rows:?}",
            );
            // The custom key of each row IS its `$from`; §14.6 requires them distinct.
            let keys: Vec<&String> = rows.iter().map(|(from, _)| from).collect();
            let distinct: BTreeSet<&&String> = keys.iter().collect();
            assert_eq!(
                distinct.len(),
                keys.len(),
                "§14.6: the custom key MUST be unique for every generated row, but two \
                 distinct generated rows {rows:?} resolve the same custom key `$from`, and \
                 admission did not reject the establishing seed — the uniqueness pass only \
                 enumerated 2 periods per source and never reached s1's period 2",
            );
        }
    }
}

// ── CONTROL: the §14.6 example composite key `["$source.id", "$from"]` ─────────
// The same two-subscription grid, but the source id disambiguates each generated
// row, so every custom key is genuinely unique across the whole series. The seed
// loads and the same collision window returns two rows with DISTINCT custom keys.
// This isolates the defect: only the single-component `["$from"]` key collides;
// adding the disambiguating `$source.id` component (the §14.6 example) is unique.
#[test]
fn composite_key_stays_unique_across_aligned_grids() {
    let definition =
        definition("t.w3b146.compositectl@1.0.0", r#""$key": ["$source.id", "$from"],"#);
    let mut engine =
        load("w3-b146-composite", &definition).expect("the unique composite-key form must load (§14.6)");
    let rows = collide_rows(&mut engine, T0 + 14 * DAY, T0 + 21 * DAY);
    assert_eq!(rows.len(), 2, "both generated rows returned, got {rows:?}");
    // The composite key is (sid, from); its full tuple must be unique per row.
    let distinct: BTreeSet<&(String, String)> = rows.iter().collect();
    assert_eq!(
        distinct.len(),
        rows.len(),
        "§14.6: the composite key ($source.id, $from) is unique for every generated row, {rows:?}",
    );
}
