//! §12.2 view-result delivery shape, driven against the real runtime + surface
//! stack over an in-memory store.
//!
//! §12.2: "a single-row or scalar result [is delivered] as one object rather
//! than a one-element array." Every expectation here is deducible from SPEC.md:
//!
//! - A root/struct projection (`. { title }`) is a single row → one JSON object.
//! - A collection source (`.items { id }`) is a row stream → a JSON array, even
//!   when it holds exactly one row.
//! - A single-key collection *selection* (`.items['a']`) is §6.3 a one-row
//!   context, yet the runtime materializes a selection as a collection cell
//!   delivered as an array — not an object (a corpus expectation, and the
//!   distinction the adapter must not collapse).
//!
//! These are not tautological: the object-vs-array split is fixed by §12.2's
//! delivery rule, not by observing the engine.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, Outcome, ScenarioAdapter, SuiteKind};

/// A package with a scalar root field, a keyed collection, and three surface
/// views over them: a root projection, a whole-collection source, and a
/// single-key selection.
const APP: &str = r##"{
  format: 1
  name: adapter-view-shape
  suite: scenario
  spec: ["#clients"]
  package: {
    $liasse: 1
    $app: "t.viewshape@1.0.0"
    $model: {
      title: "text"
      items: { $key: "id", id: "text", n: "int" }
      $public: {
        header: { $view: ". { title }" }
        list:   { $view: ".items { id }" }
        one:    { $view: ".items['a'] { id }" }
      }
    }
    $data: {
      title: "Hello"
      items: { a: { n: "1" } }
    }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<view-shape>"), &BTreeSet::new()).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("adapter-view-shape"), SuiteKind::Common, &case)
}

/// Assert step `index` ran and its `expect_init` value matcher held — so a
/// wrong-shape render (object vs array) fails the step loudly.
fn assert_step(result: &CaseResult, index: usize) {
    let step = result.steps.get(index).unwrap_or_else(|| panic!("no step {index}: {:?}", result.steps));
    assert!(step.result.is_pass(), "step {index} did not pass: {:?}", result.steps);
    assert_eq!(step.observed, Some(Outcome::Ok), "step {index} observed the wrong outcome");
}

#[test]
fn root_projection_is_one_object() {
    // §12.2: `. { title }` is a single-row root projection, delivered as one
    // object — a `[{...}]` render would fail this object matcher.
    let result = run(
        r##"[
          { watch: "public.header", id: "h",
            expect_init: { value: { title: "Hello" } } }
        ]"##,
    );
    assert_step(&result, 0);
}

#[test]
fn collection_source_is_an_array_even_with_one_row() {
    // §12.2: `.items { id }` is a row stream, delivered as an array. It holds
    // exactly one row, so an object render would coincidentally look plausible —
    // the array form is what the spec pins.
    let result = run(
        r##"[
          { watch: "public.list", id: "l",
            expect_init: { value: [ { id: "a" } ] } }
        ]"##,
    );
    assert_step(&result, 0);
}

#[test]
fn single_key_selection_is_an_array_not_an_object() {
    // §6.3 types a single-key selection as a one-row context, but the runtime
    // delivers a *selection* as a collection cell (array). The adapter must not
    // collapse it to an object.
    let result = run(
        r##"[
          { watch: "public.one", id: "o",
            expect_init: { value: [ { id: "a" } ] } }
        ]"##,
    );
    assert_step(&result, 0);
}
