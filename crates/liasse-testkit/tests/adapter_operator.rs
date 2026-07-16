//! The scenario adapter's §23.5 trusted host-operator transition wiring, driven
//! against the real runtime + surface stack over an in-memory store.
//!
//! An operator step names a bare model `$mut` (`try_write`), not a `public.*`
//! surface address. The surface router routes only surface-declared mutations, so
//! the adapter's load pass injects a synthetic public surface exposing every
//! top-level `$mut`; the operator entry ([`SurfaceHost::operator_call`]) then
//! resolves the bare name and admits it through the ordinary pipeline, bypassing
//! surface role authentication but keeping type rules, assertions, and atomicity.
//!
//! Every expected outcome is deducible from SPEC.md alone:
//!
//! - §23.5: an operator transition commits through the same admission pipeline a
//!   client call uses, so a valid transition commits and its write becomes visible
//!   through a public view.
//! - §8.8/§22.2: a mid-program `assert` failure rejects the whole transition and
//!   leaves prior committed state intact — so a failed operator write leaves no
//!   partial row, and re-running the same program after the gate opens commits on
//!   the previously contended key.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, Outcome, ScenarioAdapter, SuiteKind};

/// The chapter-local step keys these cases use (`operator` is chapter-scoped).
fn allowed() -> BTreeSet<String> {
    BTreeSet::from(["operator".to_owned()])
}

/// Build and run a scenario case from its Hjson text against the real adapter.
fn run(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<adapter-operator>"), &allowed()).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("adapter-operator"), SuiteKind::Common, &case)
}

/// Assert step `index` ran, its expectation held, and it observed `expected`.
fn assert_step(result: &CaseResult, index: usize, expected: Outcome) {
    let step = result.steps.get(index).unwrap_or_else(|| panic!("no step {index}: {:?}", result.steps));
    assert!(step.result.is_pass(), "step {index} did not pass: {:?}", result.steps);
    assert_eq!(step.observed, Some(expected), "step {index} observed the wrong outcome");
}

#[test]
fn operator_commits_an_unexposed_root_mutation_and_write_is_visible() {
    // §23.5: `add` is a model `$mut` with no `$public`/`$roles` surface, so a
    // client could not call it — yet an operator transition commits it. The
    // committed row is then visible through the public view.
    let result = run(
        r##"{
          format: 1
          name: adapter-operator-commit
          suite: scenario
          spec: ["§23.5"]
          package: {
            $liasse: 1
            $app: "t.opcommit@1.0.0"
            $model: {
              notes: { $key: "id", id: "text", body: "text" }
              $mut: { add: [ ".notes + { id: @id, body: @body }", "return { id: @id }" ] }
              all: { $view: ".notes { id, body }" }
              $public: { notes: { $view: ".all" } }
            }
          }
          steps: [
            { operator: { call: "add", args: { id: "n1", body: "from operator" } },
              expect: { outcome: ok, value: { id: "n1" } } }
            { watch: "public.notes", id: "w1",
              expect_init: { value: [ { id: "n1", body: "from operator" } ] } }
          ]
        }"##,
    );
    assert_step(&result, 0, Outcome::Ok);
    assert_step(&result, 1, Outcome::Ok);
}

#[test]
fn operator_transition_is_atomic_on_a_failed_assertion() {
    // §8.8/§22.2: the assert on the closed gate fails after the `log` write is
    // proposed; the whole transition rejects and nothing lands. Opening the gate
    // through a second operator transition, the same program then commits on key
    // `e1` — only possible if the failed run left no partial `log` row.
    let result = run(
        r##"{
          format: 1
          name: adapter-operator-atomic
          suite: scenario
          spec: ["§23.5", "§8.8", "§22.2"]
          package: {
            $liasse: 1
            $app: "t.opatomic@1.0.0"
            $model: {
              log: { $key: "id", id: "text", note: "text" }
              gate: { $key: "id", id: "text", open: "bool" }
              $mut: {
                try_write: [
                  ".log + { id: @id, note: @note }"
                  "assert(.gate[@gate].open, 'gate closed')"
                  "return { id: @id }"
                ]
                open_gate: [ ".gate[@gate].open = true", "return { id: @gate }" ]
              }
            }
            $data: { gate: { g1: { open: false } } }
          }
          steps: [
            { operator: { call: "try_write", args: { id: "e1", note: "hi", gate: "g1" } },
              expect: { outcome: rejected } }
            { operator: { call: "open_gate", args: { gate: "g1" } },
              expect: { outcome: ok, value: { id: "g1" } } }
            { operator: { call: "try_write", args: { id: "e1", note: "final", gate: "g1" } },
              expect: { outcome: ok, value: { id: "e1" } } }
          ]
        }"##,
    );
    assert_step(&result, 0, Outcome::Rejected);
    assert_step(&result, 1, Outcome::Ok);
    assert_step(&result, 2, Outcome::Ok);
}
