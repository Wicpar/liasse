//! RED-TEAM probe of the temporal `rebase_scopes` spine (§7.1 filter/projection
//! narrowing re-applied to a bucketed base's extant, §14.1 active-at selection).
//!
//! The fix in 70e1666 re-applies a filtered/projected temporal base's transform to
//! the collection's extant. This battery hunts residual gaps on the edges the
//! coordinator flagged: filter AND projection combined, nested filters, `.$all`
//! and `.$between` over a filtered base, and a filter that excludes every row.
//!
//! Every expectation is deducible from SPEC.md text alone: §7.1 (a view ranges over
//! the collection it names; a filter narrows WHICH rows, a projection RESHAPES them)
//! composed with §14.1 (`.$at`/`.$between`/`.$all` select active rows). None of these
//! cases can misattribute a row from another collection (a single bucket), so any
//! extra/leaked row is a pure predicate/projection bypass.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, Outcome, ScenarioAdapter, SuiteKind};

// Two rows share the interval [2025-06-01, 2027-01-01): both active at the
// 2026-01-01 clock and at the 2026-03-15 read instant. `sk` is the KEPT row,
// `sx` the row the filter must exclude.
const APP: &str = r##"{
  format: 1
  name: temporal-rebase-probe
  suite: scenario
  spec: ["#views", "#buckets"]
  package: {
    $liasse: 1
    $app: "t.trp@1.0.0"
    $model: {
      sessions: {
        $key: "id"
        $bucket: { $from: ".starts", $until: ".ends" }
        id: "text"
        label: "text"
        keep: "bool"
        starts: "timestamp"
        ends: "timestamp"
      }
      $public: {
        // filter AND projection both inside the temporal base's spine.
        filt_proj: {
          $params: { t: "timestamp" }
          $view: '''.sessions[:s | s.keep] { label }.$at(@t)'''
        }
        // nested filter: second predicate references a field, over the first.
        nested_filt: {
          $params: { t: "timestamp" }
          $view: '''.sessions[:s | s.keep][:u | u.label == "keep"] { label }.$at(@t)'''
        }
        // `.$all` over a filtered base (not just `.$at`).
        all_filt: {
          $view: '''.sessions[:s | s.keep] { label }.$all'''
        }
        // `.$between` over a filtered base.
        between_filt: {
          $params: { a: "timestamp", b: "timestamp" }
          $view: '''.sessions[:s | s.keep] { label }.$between(@a, @b)'''
        }
        // filter that excludes EVERY row: must yield the empty view, never the extant.
        excl_all: {
          $params: { t: "timestamp" }
          $view: '''.sessions[:s | s.label == "___never___"] { label }.$at(@t)'''
        }
        // projected base that RENAMES and COMPUTES: reshape must apply to the extant.
        proj_compute: {
          $params: { t: "timestamp" }
          $view: '''.sessions { tag: label, shout: label + "!" }.$at(@t)'''
        }
      }
    }
    $data: {
      sessions: {
        sk: { label: "keep", keep: true,  starts: "1748736000000000", ends: "1798761600000000" }
        sx: { label: "leak", keep: false, starts: "1748736000000000", ends: "1798761600000000" }
      }
    }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<temporal-rebase-probe>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("temporal-rebase-probe"), SuiteKind::Red, &case)
}

fn assert_step(result: &CaseResult, index: usize) {
    let step = result
        .steps
        .get(index)
        .unwrap_or_else(|| panic!("no step {index}: {:?}", result.steps));
    assert!(step.result.is_pass(), "step {index} did not pass: {:?}", step.result);
    assert_eq!(step.observed, Some(Outcome::Ok), "step {index} observed wrong outcome");
}

#[test]
fn filter_and_projection_in_temporal_base() {
    let result = run(
        r##"[
          { watch: "public.filt_proj", args: { t: "1773532800000000" }, id: "w1",
            expect_init: { value: [ { label: "keep" } ] } }
        ]"##,
    );
    assert_step(&result, 0);
}

#[test]
fn nested_filter_in_temporal_base() {
    let result = run(
        r##"[
          { watch: "public.nested_filt", args: { t: "1773532800000000" }, id: "w1",
            expect_init: { value: [ { label: "keep" } ] } }
        ]"##,
    );
    assert_step(&result, 0);
}

#[test]
fn all_over_filtered_base() {
    let result = run(
        r##"[
          { watch: "public.all_filt", id: "w1",
            expect_init: { value: [ { label: "keep" } ] } }
        ]"##,
    );
    assert_step(&result, 0);
}

#[test]
fn between_over_filtered_base() {
    let result = run(
        r##"[
          { watch: "public.between_filt", args: { a: "1773532800000000", b: "1781568000000000" }, id: "w1",
            expect_init: { value: [ { label: "keep" } ] } }
        ]"##,
    );
    assert_step(&result, 0);
}

#[test]
fn filter_excluding_all_rows_in_temporal_base() {
    let result = run(
        r##"[
          { watch: "public.excl_all", args: { t: "1773532800000000" }, id: "w1",
            expect_init: { value: [] } }
        ]"##,
    );
    assert_step(&result, 0);
}

#[test]
fn projection_renames_and_computes_over_temporal_base() {
    let result = run(
        r##"[
          { watch: "public.proj_compute", args: { t: "1773532800000000" }, id: "w1",
            expect_init: { value: { $unordered: [
              { tag: "keep", shout: "keep!" }
              { tag: "leak", shout: "leak!" }
            ] } } }
        ]"##,
    );
    assert_step(&result, 0);
}
