//! The scenario adapter's §9/§22 op steps — `restart` and `host_load` — driven
//! against the real runtime + surface stack over an in-memory store.
//!
//! Every expectation is deducible from SPEC.md:
//!
//! - §22: a `restart` rebuilds the host over the same engine, so committed state
//!   (including a generated key) survives unchanged and a fresh connection reads
//!   it. No `$data` seed is re-applied.
//! - §9.2/§20.1: a `host_load` of a compatible new version migrates committed
//!   state; a newly added field with a default resolves to that default for the
//!   existing row (§5.1), and the reloaded surface exposes it.
//!
//! These are not tautological: what survives a restart, and what a compatible
//! migration produces, are fixed by the spec, not by observing the engine.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, Outcome, ScenarioAdapter, SuiteKind};

/// A minimal task app: a keyed collection with a generated uuid key, a root
/// mutation that inserts one, and a public view over it.
const APP: &str = r##"{
  format: 1
  name: adapter-ops
  suite: scenario
  spec: ["#runtime"]
  package: {
    $liasse: 1
    $app: "t.adapterops@1.0.0"
    $model: {
      tasks: { $key: "id", id: "uuid = uuid()", title: "text" }
      $mut: {
        add: [ "t = .tasks + { title: @title }", "return t { id, title }" ]
      }
      $public: {
        tasks: { $view: ".tasks { title }" }
        board: { $mut: { add: ".add" } }
      }
    }
    $data: {}
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<adapter-ops>"), &BTreeSet::new()).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("adapter-ops"), SuiteKind::Common, &case)
}

fn assert_step(result: &CaseResult, index: usize, expected: Outcome) {
    let step = result.steps.get(index).unwrap_or_else(|| panic!("no step {index}: {:?}", result.steps));
    assert!(step.result.is_pass(), "step {index} did not pass: {:?}", result.steps);
    assert_eq!(step.observed, Some(expected), "step {index} observed the wrong outcome");
}

#[test]
fn committed_state_survives_a_restart() {
    // §22: add a task, restart, and read the view on a fresh connection — the
    // committed row survives. A restart that dropped committed state, or one that
    // re-applied the (empty) `$data` seed, would show no row and fail the matcher.
    let result = run(
        r##"[
          { connect: "c1" }
          { call: "public.board.add", args: { title: "durable" }, on: "c1",
            expect: { outcome: ok } }
          { restart: true }
          { connect: "c2" }
          { watch: "public.tasks", id: "w", on: "c2",
            expect_init: { value: [ { title: "durable" } ] } }
        ]"##,
    );
    assert_step(&result, 2, Outcome::Ok);
    assert_step(&result, 4, Outcome::Ok);
}

/// [`APP`] republished at 1.1.0 with an added `done` field defaulting to `false`
/// and exposed by the view — a compatible MINOR migration (Annex E.5).
const APP_V2: &str = r##"host_load: {
        package: {
          $liasse: 1
          $app: "t.adapterops@1.1.0"
          $model: {
            tasks: { $key: "id", id: "uuid = uuid()", title: "text", done: "bool = false" }
            $mut: {
              add: [ "t = .tasks + { title: @title }", "return t { id, title }" ]
            }
            $public: {
              tasks: { $view: ".tasks { title, done }" }
              board: { $mut: { add: ".add" } }
            }
          }
        }
      }"##;

#[test]
fn host_load_migrates_added_field_to_its_default() {
    // §9.2/§20.1: reload a compatible 1.1.0 that adds `done: bool = false`; the
    // pre-existing task migrates, and §5.1 supplies the default `false`, which the
    // reloaded view exposes. A stale router or an unmigrated row would drop `done`
    // or the whole task and fail the matcher.
    let steps = format!(
        r##"[
          {{ connect: "c1" }}
          {{ call: "public.board.add", args: {{ title: "x" }}, on: "c1",
            expect: {{ outcome: ok }} }}
          {{ {APP_V2},
            expect: {{ outcome: ok, result: committed }} }}
          {{ watch: "public.tasks", id: "w", on: "c1",
            expect_init: {{ value: [ {{ title: "x", done: false }} ] }} }}
        ]"##
    );
    let result = run(&steps);
    assert_step(&result, 2, Outcome::Ok);
    assert_step(&result, 3, Outcome::Ok);
}
