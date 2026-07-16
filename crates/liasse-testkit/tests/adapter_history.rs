//! The scenario adapter's §19 host operations — `export`/`import`/`reconcile` and
//! the `in_sandbox` `restore` isolation — driven against the real runtime +
//! surface stack over an in-memory store.
//!
//! Every expectation is deducible from SPEC.md, not from observing the engine:
//!
//! - §19.5/§19.10: an `export` captures the committed boundary as `.liasse` bytes
//!   a later step consumes; a `restore` inside an `in_sandbox` group activates an
//!   *isolated* instance, so mutating it leaves the outer instance's committed
//!   state exactly as it was.
//! - §19.8: an `import` of an earlier boundary that a `rollback` policy permits
//!   moves live committed state back to that boundary.
//! - §19.9: a `reconcile` whose two sides changed the same coordinate to
//!   incompatible values reports a conflict and does not activate — committed
//!   state is unchanged and the merge is not applied.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, Outcome, ScenarioAdapter, SuiteKind};

/// A minimal notes app: a keyed collection, an insert and a field-set mutation,
/// and a public collection view over it.
const APP: &str = r##"{
  format: 1
  name: adapter-history
  suite: scenario
  spec: ["§19"]
  package: {
    $liasse: 1
    $app: "t.hist@1.0.0"
    $model: {
      notes: { $key: "id", id: "text", body: "text" }
      $mut: {
        put: [ "n = .notes + { id: @id, body: @body }", "return n { id, body }" ]
        set_body: [ ".notes[@id].body = @body", "return .notes[@id] { id, body }" ]
      }
      all: { $view: ".notes { id, body, $sort: [id] }" }
      $public: {
        notes: { $view: ".all", $mut: { put: ".put", set_body: ".set_body" } }
      }
    }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    // `in_sandbox`/`restore` are chapter-scoped step keys (the corpus documents
    // them in the chapter NOTES.md); allow them for this synthetic case.
    let allowed: BTreeSet<String> = ["in_sandbox", "restore"].into_iter().map(String::from).collect();
    let case = Case::from_hjson(&text, Path::new("<adapter-history>"), &allowed).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("adapter-history"), SuiteKind::Common, &case)
}

fn assert_pass(result: &CaseResult, index: usize) {
    let step = result.steps.get(index).unwrap_or_else(|| panic!("no step {index}: {:?}", result.steps));
    assert!(step.result.is_pass(), "step {index} did not pass: {:?}", result.steps);
}

#[test]
fn sandbox_restore_does_not_perturb_the_outer_instance() {
    // §19.10: export the boundary, restore it into a sandbox, and mutate the
    // sandbox. Because the sandbox is an isolated instance, the outer instance's
    // committed state is unchanged — the final view still reads `start`. A flattened
    // sandbox sharing the base instance would show `sandbox-edit` and fail here.
    let result = run(
        r##"[
          { call: "public.notes.put", args: { id: "n1", body: "start" },
            expect: { outcome: ok, value: { id: "n1", body: "start" } } }
          { export: { as: "a0" }, expect: { outcome: ok } }
          { in_sandbox: "s1", steps: [
            { restore: { from: "a0" }, expect: { outcome: ok } }
            { call: "public.notes.set_body", args: { id: "n1", body: "sandbox-edit" },
              expect: { outcome: ok } }
          ] }
          { watch: "public.notes", id: "w1", expect_init: { value: [
            { id: "n1", body: "start" }
          ] } }
        ]"##,
    );
    for index in 0..=4 {
        assert_pass(&result, index);
    }
    assert_eq!(result.steps.get(4).map(|step| step.observed), Some(Some(Outcome::Ok)));
}

#[test]
fn import_rollback_restores_the_earlier_boundary() {
    // §19.8: export a boundary with one note, commit a second, then import the
    // earlier boundary under a `rollback` policy — live state moves back to a
    // single note. Without the movement the view would still show two notes.
    let result = run(
        r##"[
          { call: "public.notes.put", args: { id: "n1", body: "one" },
            expect: { outcome: ok } }
          { export: { as: "a1" }, expect: { outcome: ok } }
          { call: "public.notes.put", args: { id: "n2", body: "two" },
            expect: { outcome: ok } }
          { import: { from: "a1", policy: ["rollback"] },
            expect: { outcome: ok, value: { relation: "rollback", applied: true, "...": true } } }
          { watch: "public.notes", id: "w1", expect_init: { value: [
            { id: "n1", body: "one" }
          ] } }
        ]"##,
    );
    for index in 0..=4 {
        assert_pass(&result, index);
    }
}

#[test]
fn reconcile_incompatible_edits_conflicts_and_leaves_state() {
    // §19.9: both sides edit the same field to different values. The reconcile
    // reports a conflict and does not activate; committed state keeps the local
    // edit. The merge base is the artifact the sandbox restored from, which the
    // adapter tracks so the reconcile step need not name it.
    let result = run(
        r##"[
          { call: "public.notes.put", args: { id: "n1", body: "start" },
            expect: { outcome: ok } }
          { export: { as: "a0" }, expect: { outcome: ok } }
          { in_sandbox: "s1", steps: [
            { restore: { from: "a0" }, expect: { outcome: ok } }
            { call: "public.notes.set_body", args: { id: "n1", body: "incoming" },
              expect: { outcome: ok } }
            { export: { as: "a2" }, expect: { outcome: ok } }
          ] }
          { call: "public.notes.set_body", args: { id: "n1", body: "local" },
            expect: { outcome: ok } }
          { reconcile: { from: "a2", policy: ["merge"] },
            expect: { outcome: ok, value: { relation: "merge", applied: false, conflicts: "$any", "...": true } } }
          { watch: "public.notes", id: "w1", expect_init: { value: [
            { id: "n1", body: "local" }
          ] } }
        ]"##,
    );
    for index in 0..=7 {
        assert_pass(&result, index);
    }
}
