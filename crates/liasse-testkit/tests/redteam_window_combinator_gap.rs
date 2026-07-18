//! RED-TEAM bug reproduction (SPEC §12.2 live-view coherence × §7.4 combinators).
//!
//! A bounded window's frozen-gap `resume` partitions the current rows through the
//! view's declared total order (§12.2). The engine derives that order from the
//! OUTERMOST typed node only: `TypedExpr::result_order`
//! (`crates/liasse-expr/src/typed.rs:85`) returns the projection's `$sort`
//! directions for a `Project`, and `SortOrder::unordered()` (occurrence-identity /
//! Annex B.5 order) for EVERYTHING ELSE — including a view combinator.
//!
//! But §7.4 pins a combinator's order to its LEFT operand:
//!
//! > `a - b`   difference, left projection and order
//! > `a & b`   intersection, left projection and order
//! > `a | b`   union, left order then new right identities
//!
//! So a difference whose left operand is a DESCENDING `$sort` projection is
//! delivered in descending id order, yet the runtime tags the result with
//! `SortOrder::unordered()` (ascending occurrence identity). When a windowed
//! subscription's anchor leaves the view, `FrozenGap::resume`
//! (`crates/liasse-surface/src/window.rs:71`) calls `rows.partition_point(..)`
//! with that contradictory order. `partition_point` presupposes the slice is
//! monotone under the predicate; descending rows are NOT monotone under an
//! ascending occurrence-identity comparator, so the binary search returns a
//! meaningless index and the window COLLAPSES.
//!
//! This violates §12.2: "After applying every operation, the client result MUST
//! equal the authorized declared view at the new frontier." The window ships an
//! EMPTY result where the authorized declared window is non-empty.
//!
//! Every expectation below is deducible from SPEC.md text alone (§7.4 difference
//! order + §12.2 gap resume), mirroring the existing ASCENDING analogue
//! `tests/12-clients-live-views/red/window-anchor-gap-and-reappearance.hjson`,
//! which passes only because ascending id order coincides with occurrence order.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, Outcome, ScenarioAdapter, SuiteKind};

/// Four id-keyed rows behind a PUBLIC view that is a `difference` combinator
/// (`.desc - .never`) whose left operand sorts DESCENDING by id and whose right
/// operand is always empty. The result is therefore `.desc` verbatim — rows in
/// descending id order `[d, c, b, a]` — but its outermost typed node is a
/// combinator, so `result_order()` reports `unordered()`.
const APP: &str = r##"{
  format: 1
  name: window-combinator-gap
  suite: scenario
  spec: ["#clients", "#views"]
  package: {
    $liasse: 1
    $app: "t.wcg@1.0.0"
    $model: {
      items: { $key: "id", id: "text", done: "bool = false" }
      $mut: { complete: ".items[@id].done = true" }
      desc:  { $view: ".items[:i | !i.done] { id, $sort: [-id] }" }
      never: { $view: ".items[:i | i.id == '___never___'] { id, $sort: [-id] }" }
      $public: {
        items: { $view: ".desc - .never", $mut: { complete: ".complete" } }
      }
    }
    $data: { items: { a: {}, b: {}, c: {}, d: {} } }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<window-combinator-gap>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("window-combinator-gap"), SuiteKind::Red, &case)
}

fn assert_ok_step(result: &CaseResult, index: usize) {
    let step = result
        .steps
        .get(index)
        .unwrap_or_else(|| panic!("no step {index}: {:?}", result.steps));
    assert!(
        step.result.is_pass(),
        "step {index} did not pass: {:?}",
        step.result
    );
    assert_eq!(step.observed, Some(Outcome::Ok), "step {index} observed the wrong outcome");
}

/// GUARD (passes today): the authorized, unwindowed recompute of the combinator
/// view is correct. After completing "c" the difference view is the descending
/// `[d, b, a]` (§7.4 left order). This isolates the defect to the WINDOW/order
/// layer — the recompute itself is sound.
#[test]
fn full_view_recompute_stays_correct() {
    let result = run(
        r##"[
          { connect: "c1" }
          { watch: "public.items", on: "c1", id: "wf",
            expect_init: { value: [ { id: "d" }, { id: "c" }, { id: "b" }, { id: "a" } ] } }
          { call: "public.items.complete", args: { id: "c" }, on: "c1", expect: { outcome: ok } }
          { expect_view: { watch: "wf", value: [ { id: "d" }, { id: "b" }, { id: "a" } ] } }
        ]"##,
    );
    for index in 0..4 {
        assert_ok_step(&result, index);
    }
}

/// BUG (fails today): a bounded window over the SAME combinator view.
///
/// Open a size-2 window anchored on "c": the descending view is `[d, c, b, a]`,
/// so the anchor's window is `[c, b]` (§12.2 "a concrete anchor normally becomes
/// the first row"). Then complete "c": the anchor leaves the view, and §12.2's
/// frozen gap coordinate "c" determines the window — the first 2 rows AT OR AFTER
/// c's position in the view's own (descending, §7.4) order, i.e. `[b, a]`.
///
/// The runtime instead collapses the window to EMPTY, because the gap `resume`
/// partitions the descending rows through `SortOrder::unordered()` (see the module
/// header). The final `expect_view` therefore fails, reproducing the §12.2
/// coherence violation.
#[test]
fn windowed_gap_over_combinator_view_stays_coherent() {
    let result = run(
        r##"[
          { connect: "c1" }
          { watch: "public.items", on: "c1", id: "w1",
            window: { $size: 2, $anchor: "c" },
            expect_init: { value: [ { id: "c" }, { id: "b" } ] } }
          { call: "public.items.complete", args: { id: "c" }, on: "c1", expect: { outcome: ok } }
          { expect_view: { watch: "w1", value: [ { id: "b" }, { id: "a" } ] } }
        ]"##,
    );

    // Steps 0-2 (connect, windowed open at [c,b], commit) are sound today.
    for index in 0..3 {
        assert_ok_step(&result, index);
    }

    // Step 3 is the §12.2 coherence assertion the runtime currently violates:
    // the gap-resumed window MUST equal the authorized declared window `[b, a]`,
    // but the combinator's `unordered()` order collapses it (observed: empty).
    let gap = result.steps.get(3).expect("gap expect_view step ran");
    assert!(
        gap.result.is_pass(),
        "§12.2/§7.4 live-view coherence violated: a bounded window over a difference \
         combinator (descending left order) collapses at its frozen gap instead of \
         resuming to the authorized `[b, a]`. Root cause: `result_order()` reports \
         `unordered()` for the combinator node while its rows are descending, so \
         `FrozenGap::resume`'s `partition_point` runs over a non-monotone slice. \
         verdict: {:?}",
        gap.result
    );
}
