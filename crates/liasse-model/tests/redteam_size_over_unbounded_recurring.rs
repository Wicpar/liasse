//! RED-TEAM (§14.5): `size()` over an unbounded recurring source-backed bucket is
//! NOT rejected, though its aggregate twin `count()` is.
//!
//! §14.5 (SPEC.md): "An unbounded recurring collection MUST be read through a
//! bounded temporal selector such as `.$at` or `.$between`. The checker rejects an
//! expression requiring enumeration of an infinite series."
//!
//! `size(collection)` is this implementation's documented rows-count of a view
//! (crates/liasse-model/src/check.rs: "a scalar ... like `= size(.docs)`"; it
//! evaluates as `Cell::Collection(rows) => rows.len()` in
//! crates/liasse-expr/src/eval/builtins.rs). Counting every row of a collection is
//! exactly the enumeration `count(view)` performs — and `count(.credit_periods)`
//! IS rejected at load by the §14.5 guard in `check_aggregate`
//! (crates/liasse-expr/src/check/views.rs). `size()` reaches the checker through
//! the separate `check_builtin` path (crates/liasse-expr/src/check/views.rs:250),
//! which never inspects the row's unbounded-recurring marker, so
//! `size(.credit_periods)` loads. That asymmetry is the defect: two spellings of
//! "count all rows of this view", one guarded and one not.
//!
//! The three assertions isolate it: the bounded `size(...$at(t))` control loads,
//! the `count(.credit_periods)` control is rejected (guard present for the twin),
//! and only the bare `size(.credit_periods)` assertion — the one this file exists
//! to prove — currently fails, because the model accepts it.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// The shared model: an UNBOUNDED source-backed recurring bucket `credit_periods`
/// (optional `ends_at`, a possibly-non-none plan `period`), so reading
/// `.credit_periods` whole enumerates an infinite series (§14.5). `read` exposes a
/// `timestamp` param so a case can bound with `.$at`. This is the exact package
/// shape the sibling `unbounded_marker_propagation.rs` battery uses.
fn package(view: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "redteam.sizeunbounded@1.0.0",
          "$model": {{
            "plans": {{ "$key": "id", "id": "text", "credits": "decimal", "period": "period?" }},
            "subscriptions": {{
              "$key": "id", "id": "text", "plan": {{ "$ref": "/plans" }},
              "starts_at": "timestamp", "ends_at": "timestamp?"
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
            "$public": {{
              "read": {{ "$params": {{ "t": "timestamp" }}, "$view": "{view}" }}
            }}
          }}
        }}"#
    )
}

/// CONTROL (guard present): `count(.credit_periods)` reduces the whole
/// possibly-infinite series to a scalar with no bounding selector, so §14.5 must
/// reject it — and does. Establishes that the guard exists for the aggregate twin,
/// so the `size` divergence below is a missed path, not a missing feature.
#[test]
fn count_over_unbounded_is_rejected_control() {
    assert!(
        build(&package("count(.credit_periods)")).result.is_err(),
        "control: `count` over an unbounded recurring bucket must reject (§14.5)"
    );
}

/// CONTROL (not over-rejected): a bounded `size(.credit_periods.$at(@t))` reads a
/// finite temporal slice, so counting its rows enumerates nothing infinite and the
/// package MUST load. Guards against a fix over-rejecting the bounded form.
#[test]
fn size_over_bounded_slice_loads_control() {
    build(&package("size(.credit_periods.$at(@t))")).expect_ok();
}

/// THE DEFECT: `size(.credit_periods)` counts every row of the unbounded recurring
/// bucket — the same infinite-series enumeration `count(.credit_periods)` performs
/// (rejected above) — yet it loads, because `check_builtin` never consults the
/// §14.5 unbounded-recurring marker the aggregate path checks. §14.5 requires the
/// checker to reject an expression requiring enumeration of an infinite series, so
/// this read MUST be rejected at load. This assertion currently FAILS, reproducing
/// the bug.
#[test]
fn size_over_unbounded_is_rejected() {
    assert!(
        build(&package("size(.credit_periods)")).result.is_err(),
        "`size(.credit_periods)` counts every row of an unbounded recurring bucket \
         (the same enumeration the rejected `count(.credit_periods)` performs); \
         §14.5 requires the checker to reject a whole-series read with no bounding \
         temporal selector — but the model accepts it"
    );
}
