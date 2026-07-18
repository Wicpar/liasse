//! RED-TEAM probe (Target 2): the ff4cf74 in-place `SurfaceHost::update` sweeps
//! the ALREADY-OPEN subscription at the migration frontier. Does a pre-existing
//! watcher held ACROSS a projection-changing migration get a coherent patch
//! reflecting the NEW authorized projection (§12.2), or a stale one?
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

fn run(case_text: &str, name: &str) -> CaseResult {
    let case = Case::from_hjson(case_text, Path::new(name), &BTreeSet::new()).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new(name), SuiteKind::Red, &case)
}

fn assert_steps_pass(result: &CaseResult, upto: usize, context: &str) {
    for index in 0..upto {
        let step = result.steps.get(index).unwrap_or_else(|| panic!("{context}: no step {index}"));
        assert!(step.result.is_pass(), "{context}: step {index} ({}) did not pass: {:?}", step.action, step.result);
    }
}

/// §9.3 × §12.2 × §20.1: a live watch `w1` on `public.people` is held ACROSS a
/// migration that WIDENS the surface projection ({id,name} -> {id,name,tier}) and
/// seeds the added field with a default. The definition update is a commit (§9.3);
/// the ff4cf74 sweep must recompute `w1` at the outgoing frontier so `expect_view`
/// on the SAME (never re-opened) subscription reflects the new projection.
#[test]
fn open_watcher_swept_reflects_widened_projection() {
    let case = r##"{
      format: 1
      name: migration-sweep-widened-projection
      suite: scenario
      spec: ["#loading", "§9.3", "#clients", "§12.2", "#evolution", "§20.1"]
      package: {
        $liasse: 1
        $app: "t.msp@1.0.0"
        $model: {
          people: { $key: "id", id: "text", name: "text" }
          $public: { people: { $view: ".people { id, name, $sort: [id] }" } }
        }
        $data: { people: { p1: { name: "bob" } } }
      }
      steps: [
        { watch: "public.people", id: "w1",
          expect_init: { value: [ { id: "p1", name: "bob" } ] } }
        { host_load: { package: {
            $liasse: 1
            $app: "t.msp@2.0.0"
            $model: {
              people: { $key: "id", id: "text", name: "text", tier: "text = 'gold'" }
              $public: { people: { $view: ".people { id, name, tier, $sort: [id] }" } }
            }
          } }, expect: { outcome: ok, result: committed } }
        { expect_view: { watch: "w1", value: [ { id: "p1", name: "bob", tier: "gold" } ] } }
      ]
    }"##;
    let result = run(case, "migration-sweep-widened-projection");
    assert_steps_pass(&result, 3, "open watcher swept reflects widened projection");
}
