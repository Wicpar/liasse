#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §14.5 bounded temporal read of an UNBOUNDED recurring source-backed bucket.
//!
//! A recurring source-backed bucket with no series `$until` generates periods
//! indefinitely; §14.5 permits reading it only through a bounded temporal selector
//! (`.$at`/`.$between`), whose own instant/window is the bound that makes the read
//! finite. That bound MUST drive how far the series is generated: a `.$at(t)`/
//! `.$between(a, b)` whose instant/window lies past the engine clock still has to
//! generate the periods that cover it. The clock stays fixed at genesis (T0) in
//! every case, so each result follows only from the selector's argument — never
//! from the clock — which is exactly what makes the future-read assertions
//! externally deducible from §14.1/§14.5 rather than from engine behaviour.
//!
//! The weekly (fixed `P7D`) and monthly (calendar `{ months: 1 }`) variants share
//! one root cause (the generation horizon), so one fix covers both; each is
//! asserted independently.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Precision, Timestamp, Value};
use support::store;

/// 2026-01-01T00:00:00Z in microseconds — genesis T0, the fixed engine clock and
/// the instant every subscription's series begins, so period index 0 is active at
/// the clock.
const T0: i128 = 1_767_225_600_000_000;

/// One day in microseconds.
const DAY: i128 = 86_400_000_000;

/// 2026-03-15T00:00:00Z in microseconds (T0 + 73 days): inside the third calendar
/// month `[2026-03-01, 2026-04-01)` = monthly period index 2.
const MAR15: i128 = T0 + 73 * DAY;

/// A weekly/monthly recurring source-backed bucket over one subscription starting
/// at genesis, with root mutations that read it through a bounded temporal
/// selector. `period_json` is the plan's `period` value literal (a fixed-duration
/// string or a calendar object), so the same model exercises both recurrence
/// kinds.
fn definition(period_json: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1, "$app": "t.b145.horizon@1.0.0",
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
      "credits": "= /plans[$source.plan].credits"
    }},
    "$mut": {{
      "at({{ t: timestamp }})": "return .credit_periods.$at(@t) {{ index: $index, from: $from }}",
      "between({{ a: timestamp, b: timestamp }})": "return .credit_periods.$between(@a, @b) {{ index: $index }}"
    }}
  }},
  "$data": {{
    "plans": {{ "p": {{ "credits": "100", "period": {period_json} }} }},
    "subscriptions": {{ "s1": {{ "plan": "p", "starts_at": "1767225600000000" }} }}
  }}
}}"#
    )
}

fn at(micros: i128) -> Value {
    Value::Timestamp(Timestamp::new(micros, Precision::Micros))
}

/// A fresh engine whose clock is pinned to genesis T0, loaded from `definition`.
fn engine(instance: &str, definition: &str) -> liasse_runtime::Engine<liasse_store::MemoryStore> {
    let mut generators = liasse_runtime::FixedGenerators::new(T0, Precision::Micros);
    match liasse_runtime::Engine::load(store(instance), definition, &mut generators) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    }
}

/// The `$index` values a read-only temporal mutation returned, in result order.
fn indices(
    engine: &mut liasse_runtime::Engine<liasse_store::MemoryStore>,
    request: CallRequest,
) -> Vec<i64> {
    let mut generators = liasse_runtime::FixedGenerators::new(T0, Precision::Micros);
    let outcome = engine.call(&request, &mut generators).expect("temporal call resolves");
    assert!(
        !matches!(outcome, CallOutcome::Rejected(_)),
        "a valid bounded temporal read is not rejected, got {outcome:?}",
    );
    let response = outcome.response().expect("the read-only mutation returns a value");
    match response.to_wire() {
        serde_json::Value::Array(rows) => rows
            .into_iter()
            .map(|row| {
                let index = &row["index"];
                index
                    .as_i64()
                    .or_else(|| index.as_str().and_then(|s| s.parse().ok()))
                    .unwrap_or_else(|| panic!("period `index` is an integer, got {index}"))
            })
            .collect(),
        other => panic!("expected an array of derived rows, got {other}"),
    }
}

/// §14.5 / §14.1: reading an unbounded weekly recurring bucket through `.$at(t)`
/// must return exactly the single period active at `t` — `[bi, bi+1)` with
/// `bi <= t < bi+1` — for any `t`, INCLUDING one past the engine clock. The clock
/// stays at genesis throughout, so the future read cannot be masked by an advanced
/// clock: the covering period exists only if the selector's instant drove series
/// generation.
#[test]
fn weekly_at_generates_period_covering_a_future_instant() {
    let mut engine = engine("horizon-weekly-at", &definition("\"P7D\""));

    // CONTROL (masked the bug: a read AT the clock already worked). Period 0 is
    // active at T0: [T0, T0 + 7d).
    assert_eq!(
        indices(&mut engine, CallRequest::new("at").arg("t", at(T0))),
        vec![0],
        "period index 0 is active at genesis",
    );

    // A read 31 days out — four full weeks past the clock — is the fifth period
    // (index 4), whose week [T0 + 28d, T0 + 35d) contains T0 + 31d. Before the fix
    // this returned [] because the series was generated only up to the clock.
    assert_eq!(
        indices(&mut engine, CallRequest::new("at").arg("t", at(T0 + 31 * DAY))),
        vec![4],
        "the period active 31 days out is index 4, generated to cover the selector's instant",
    );
}

/// §14.5 / §14.1: `.$between(a, b)` over an unbounded weekly bucket must return
/// every period intersecting `[a, b)`, generating far enough to cover `b` even
/// when the window lies wholly past the engine clock. `[T0 + 28d, T0 + 42d)`
/// spans exactly weeks 4 (`[T0+28d, T0+35d)`) and 5 (`[T0+35d, T0+42d)`); week 6
/// begins at `T0+42d` and, being half-open, does not intersect.
#[test]
fn weekly_between_generates_all_periods_covering_a_future_window() {
    let mut engine = engine("horizon-weekly-between", &definition("\"P7D\""));

    assert_eq!(
        indices(
            &mut engine,
            CallRequest::new("between").arg("a", at(T0 + 28 * DAY)).arg("b", at(T0 + 42 * DAY)),
        ),
        vec![4, 5],
        "a future window returns exactly the two weeks it intersects",
    );
}

/// §14.5 / §14.7 (calendar recurrence): the same horizon rule governs a monthly
/// (`{ months: 1 }`) recurring bucket. With the clock at genesis, `.$at(2026-03-15)`
/// must return the active month period — index 2, the calendar month
/// `[2026-03-01, 2026-04-01)` anchored on T0 by `2 × 1 month` (Annex A.4) — rather
/// than the empty result the clock-bounded generation produced.
#[test]
fn monthly_at_generates_calendar_month_covering_a_future_instant() {
    let mut engine = engine("horizon-monthly-at", &definition(r#"{ "months": 1, "zone": "UTC" }"#));

    // CONTROL: the first month [2026-01-01, 2026-02-01) is active at genesis.
    assert_eq!(
        indices(&mut engine, CallRequest::new("at").arg("t", at(T0))),
        vec![0],
        "monthly period index 0 is active at genesis",
    );

    // 2026-03-15 falls in the third calendar month: index 2.
    assert_eq!(
        indices(&mut engine, CallRequest::new("at").arg("t", at(MAR15))),
        vec![2],
        "the calendar month active on 2026-03-15 is index 2, generated to cover the instant",
    );
}
