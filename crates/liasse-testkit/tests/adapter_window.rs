//! §12.2 bounded-window delivery with a concrete occurrence anchor, driven
//! against the real runtime + surface stack over an in-memory store.
//!
//! §12.2: "A concrete anchor normally becomes the first row; `$slide: true`
//! centers it as far as the view bounds allow." The adapter must resolve the
//! anchor's wire key to the same stable row identity (Annex D.2) the view
//! materializer assigns, then hand the surface a bounded window — not track the
//! whole view. Each expectation here is deducible from §12.2 and the fixed row
//! ordering, not from observing the engine.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, Outcome, ScenarioAdapter, SuiteKind};

/// A package of five id-sorted rows behind a surface view, so a bounded window
/// with a concrete anchor has room to sit on either side of its anchor.
const APP: &str = r##"{
  format: 1
  name: adapter-window
  suite: scenario
  spec: ["#clients"]
  package: {
    $liasse: 1
    $app: "t.window@1.0.0"
    $model: {
      items: { $key: "id", id: "text" }
      index: { $view: ".items { id, $sort: [id] }" }
      $public: { items: { $view: ".index" } }
    }
    $data: {
      items: { a: {}, b: {}, c: {}, d: {}, e: {} }
    }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<window>"), &BTreeSet::new()).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("adapter-window"), SuiteKind::Common, &case)
}

fn assert_step(result: &CaseResult, index: usize) {
    let step = result.steps.get(index).unwrap_or_else(|| panic!("no step {index}: {:?}", result.steps));
    assert!(step.result.is_pass(), "step {index} did not pass: {:?}", result.steps);
    assert_eq!(step.observed, Some(Outcome::Ok), "step {index} observed the wrong outcome");
}

#[test]
fn concrete_anchor_becomes_first_row() {
    // §12.2: a concrete anchor normally becomes the window's first row — anchored
    // on "c" with size 2 yields [c, d], not the whole five-row view.
    let result = run(
        r##"[
          { watch: "public.items", id: "w",
            window: { $size: 2, $anchor: "c" },
            expect_init: { value: [ { id: "c" }, { id: "d" } ] } }
        ]"##,
    );
    assert_step(&result, 0);
}

#[test]
fn sliding_anchor_centers_within_bounds() {
    // §12.2 `$slide: true`: a size-3 window centered on "c" is [b, c, d]; the
    // whole-view render (five rows) would fail this array matcher.
    let result = run(
        r##"[
          { watch: "public.items", id: "w",
            window: { $size: 3, $anchor: "c", $slide: true },
            expect_init: { value: [ { id: "b" }, { id: "c" }, { id: "d" } ] } }
        ]"##,
    );
    assert_step(&result, 0);
}

#[test]
fn anchor_with_no_current_occurrence_fails_the_window() {
    // §12.2: a concrete anchor MUST identify exactly one current occurrence at
    // open; an anchor naming an absent key opens no window (an error subscription),
    // never a silent whole-view fallback.
    let result = run(
        r##"[
          { watch: "public.items", id: "w",
            window: { $size: 2, $anchor: "zzz" },
            expect: { outcome: error } }
        ]"##,
    );
    let step = result.steps.first().expect("step 0 ran");
    assert!(step.result.is_pass(), "step 0 did not pass: {:?}", result.steps);
    assert_eq!(step.observed, Some(Outcome::Error), "an absent anchor must open an error subscription");
}
