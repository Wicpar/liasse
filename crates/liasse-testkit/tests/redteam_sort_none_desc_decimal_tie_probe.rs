//! RED-TEAM probe: one `$sort` key that combines THREE Annex-B rules the corpus
//! only exercises separately — per-key direction (§B.5 "a leading `-` reverses
//! ONE key"), optional none placement under reversal (none-last ascending →
//! none-FIRST descending), and decimal numeric equality (§B.1 "numerically equal
//! canonical values compare equal") falling through to the secondary key.
//!
//! `$sort: ["-opt", "id"]` over an optional decimal `opt`:
//!   * two rows with `opt = none` sort FIRST (descending reverses the
//!     none-last-ascending rule), tied on `opt`, so the secondary key `id`
//!     ASCENDING orders them (per-key direction: the `-` does NOT touch `id`);
//!   * the present values sort DESCENDING: `10` then `2`;
//!   * `1.0` and `1.00` are numerically EQUAL (one sort value), so they tie on
//!     `opt` and again fall to `id` ascending.
//!
//! Seed `opt` assignment is scrambled relative to the output, and the view
//! projects only `id`, so the asserted row order is fully determined by the three
//! rules above and NOT by seed order, decimal scale, or decimal text. A comparator
//! that applied `-` to the whole comparison, mis-placed none under reversal, or
//! ordered decimals by scale/text would emit a different sequence.
//!
//! Expected `-opt, id`:  k1(none) k4(none)  k3(10) k5(2)  k2(1.00) k6(1.0)
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

const APP: &str = r##"{
  format: 1
  name: sort-none-desc-decimal-tie-probe
  suite: scenario
  spec: ["#annex-b", "§B.5", "§B.1"]
  package: {
    $liasse: 1
    $app: "t.annexb.nonedesctie@1.0.0"
    $model: {
      rows: {
        $key: "id"
        id: "text"
        opt: "decimal?"
      }
      $public: {
        ranked: { $view: ".rows { id, $sort: [-opt, id] }" }
      }
    }
    $data: {
      rows: {
        k1: {}
        k2: { opt: "1.00" }
        k3: { opt: "10" }
        k4: {}
        k5: { opt: "2" }
        k6: { opt: "1.0" }
      }
    }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<sort-none-desc-decimal-tie-probe>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("annex-b-total-order"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult) {
    for (index, step) in result.steps.iter().enumerate() {
        assert!(
            step.result.is_pass(),
            "step {index} did not pass: observed={:?} result={:?}",
            step.observed,
            step.result
        );
    }
}

/// none-first (descending) with id-ascending tiebreak, present values descending,
/// and the 1.0 == 1.00 numeric tie broken by id ascending.
#[test]
fn none_first_descending_with_decimal_tie_falls_to_id() {
    let result = run(
        r##"[
          { watch: "public.ranked", id: "w1", expect_init: { value: [
            { id: "k1" }
            { id: "k4" }
            { id: "k3" }
            { id: "k5" }
            { id: "k2" }
            { id: "k6" }
          ] } }
        ]"##,
    );
    assert_all_pass(&result);
}
