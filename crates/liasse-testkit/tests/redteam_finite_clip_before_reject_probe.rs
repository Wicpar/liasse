//! RED-TEAM probe: a finite source-backed recurring series whose `$until` bound
//! CLIPS the last interval BEFORE the first `overflow: reject` boundary (§14.5/§14.7).
//!
//! §14.5: "an `overflow: reject` boundary WITHIN the enumerable series" is validated
//! eagerly. §14.7: the enumerable series of a finite bound is `[from, $until)`; its
//! final interval is clipped to `$until`, so a recurrence boundary at or after
//! `$until` is never an interval endpoint and is NOT within the enumerable series.
//!
//! A monthly `overflow: reject` subscription anchored on 2026-01-31 with a finite
//! `ends_at` of 2026-02-15 has b1 = "Feb 31" (missing). But b1's position (clamped
//! Feb 28) is past the bound Feb 15, so the single interval is clipped to
//! [Jan 31, Feb 15) and the missing boundary never appears. The subscription MUST be
//! admitted (contrast `red/overflow-reject-detection-timing`, whose bound Mar 31 lies
//! PAST b1, leaving the missing boundary inside the series and rejecting).
//!
//! Every expectation is deducible from SPEC.md alone: §14.5 (clip to `$until`,
//! "within the enumerable series"), §14.7 (finite-series enumerability), §14.1
//! (`.$between` intersects the half-open interval).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, Outcome, ScenarioAdapter, SuiteKind};

const APP: &str = r##"{
  format: 1
  name: finite-clip-before-reject-probe
  suite: scenario
  spec: ["#buckets", "§14.5", "§14.7"]
  package: {
    $liasse: 1
    $app: "t.fcbr@1.0.0"
    $model: {
      plans: {
        $key: "id"
        id: "text"
        credits: "decimal"
        period: "period?"
      }
      subscriptions: {
        $key: "id"
        id: "text"
        plan: { $ref: "/plans" }
        starts_at: "timestamp"
        ends_at: "timestamp?"
      }
      credit_periods: {
        $bucket: {
          $source: ".subscriptions"
          $from: "$source.starts_at"
          $until: "$source.ends_at"
          $repeat: "/plans[$source.plan].period"
        }
        credits: "= /plans[$source.plan].credits"
      }
      $public: {
        periods: {
          $params: { a: "timestamp", b: "timestamp" }
          $view: '.credit_periods.$between(@a, @b) { index: $index, from: $from, until: $until, $sort: ["from"] }'
        }
        subs: {
          $mut: {
            add: [
              "s = .subscriptions + { id: @id, plan: @plan, starts_at: @starts_at, ends_at: @ends_at }"
              "return s { id }"
            ]
          }
        }
      }
    }
    $data: {
      plans: {
        strict_monthly: { credits: "100", period: { months: 1, zone: "UTC", overflow: "reject" } }
      }
    }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<finite-clip-before-reject-probe>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("finite-clip-before-reject-probe"), SuiteKind::Red, &case)
}

fn assert_step_ok(result: &CaseResult, index: usize) {
    let step = result
        .steps
        .get(index)
        .unwrap_or_else(|| panic!("no step {index}: {:?}", result.steps));
    assert!(step.result.is_pass(), "step {index} did not pass: {:?}", step.result);
    assert_eq!(step.observed, Some(Outcome::Ok), "step {index} observed wrong outcome");
}

/// Assert the step's observed outcome matched the case's `expect` (whatever it was).
fn assert_step_matched(result: &CaseResult, index: usize) {
    let step = result
        .steps
        .get(index)
        .unwrap_or_else(|| panic!("no step {index}: {:?}", result.steps));
    assert!(step.result.is_pass(), "step {index} did not match its expectation: {:?}", step.result);
}

/// The subscription establishing the clipped finite series is ADMITTED, and reading
/// its bucket yields exactly the one clipped interval [Jan 31, Feb 15). Baseline
/// (pre-fix) rejects the insert because `recurring_intervals` computes the missing b1
/// (past the bound) with `?` and propagates `CalendarOverflowRejected`.
#[test]
fn finite_bound_clipping_before_reject_boundary_is_admitted_and_read() {
    let result = run(
        r##"[
          // starts_at 2026-01-31, ends_at 2026-02-15; b1 = "Feb 31" (missing) sits
          // past the Feb 15 bound, so the series is one clipped interval.
          { call: "public.subs.add"
            args: { id: "s1", plan: "strict_monthly",
                    starts_at: "1769817600000000", ends_at: "1771113600000000" }
            expect: { outcome: ok, value: { id: "s1" } } }

          // window [2026-01-01, 2026-03-01): intersects the clipped interval.
          { watch: "public.periods", args: { a: "1767225600000000", b: "1772323200000000" }, id: "w1",
            expect_init: { value: [
              { index: "0", from: "1769817600000000", until: "1771113600000000" }
            ] } }
        ]"##,
    );
    assert_step_ok(&result, 0);
    assert_step_ok(&result, 1);
}

/// Guard the complement: a finite bound (Mar 31) PAST the missing b1 leaves it inside
/// the enumerable series, so admission MUST reject (§14.7). Mirrors the resolved
/// `red/overflow-reject-detection-timing`; keeps the fix from over-admitting.
#[test]
fn finite_bound_past_missing_interior_boundary_still_rejects() {
    let result = run(
        r##"[
          { call: "public.subs.add"
            args: { id: "s1", plan: "strict_monthly",
                    starts_at: "1769817600000000", ends_at: "1774915200000000" }
            expect: { outcome: rejected, violates: ["#buckets", "§14.7"] } }
        ]"##,
    );
    assert_step_matched(&result, 0);
}
