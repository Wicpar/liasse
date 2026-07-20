//! RED-TEAM (WAVE 4) — TARGET 2: a projection with a NESTED sub-collection
//! member returns rows MISSING that member.
//!
//! SPEC §7.1 (SPEC.md:804/817): a projection member may be a "nested structure"
//! (`nested: { ... }`), and "Projection members are unordered named outputs" —
//! every declared member is an output of the row. A member whose value is a
//! sub-collection view (`accounts: .accounts { id, label }`) is therefore an
//! output member that MUST appear on every projected row, carrying the nested
//! collection's rows. §12.2 delivers that view result to a watcher verbatim.
//!
//! Wave-3 filed the ledger entry
//! `15-meters/pool-removal-preserves-recorded-funding` as a §15 mutation
//! divergence ("expected ok observed rejected"); its `audit` view is
//! `.users { id, spends: .spends { id, funding } }` — exactly this nested shape.
//! This probe isolates the projection with NO meter and NO mutation: pure seed
//! data read through a nested-collection view. A CONTROL reads the flat
//! projection (`.parents { id }`); the PROBE reads the nested projection
//! (`.parents { id, kids: .children { id, label } }`) and asserts the `kids`
//! member is present with its rows.
//!
//! Externally deducible: `p1` holds two children `c1`(x) / `c2`(y) in seed data;
//! §7.1 makes `kids` an output member of `p1`'s projected row, so the row MUST be
//! `{ id: "p1", kids: [ {id:c1,label:x}, {id:c2,label:y} ] }`. A fail whose
//! observed row omits `kids` (or empties it) is the §7/§12 member-drop.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

// Pure read model: a parent collection with a nested child collection, and two
// public views — one flat, one nesting the child sub-collection as a member.
// No meters, no mutations: the only thing under test is the projection shape.
const APP: &str = r##"{
  format: 1
  name: w4-nested-view-member-drop
  suite: scenario
  spec: ["#views", "§7.1", "#clients", "§12.2"]
  package: {
    $liasse: 1
    $app: "t.rt.nestview@1.0.0"
    $model: {
      parents: {
        $key: "id"
        id: "text"
        children: {
          $key: "id"
          id: "text"
          label: "text"
        }
      }
      $public: {
        flat: { $view: ".parents { id }" }
        tree: { $view: ".parents { id, kids: .children { id, label } }" }
      }
    }
    $data: { parents: { p1: { children: { c1: { label: "x" }, c2: { label: "y" } } } } }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<w4-nested-view-member-drop>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("07-views"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult, label: &str) {
    for (index, step) in result.steps.iter().enumerate() {
        assert!(
            step.result.is_pass(),
            "[{label}] step {index} did not pass: observed={:?} result={:?}",
            step.observed,
            step.result
        );
    }
}

/// PASSING CONTROL: the flat projection `.parents { id }` delivers the parent
/// row with its `id`. Establishes the view/watch path and the seed are sound so
/// the probe's only variable is the nested member.
#[test]
fn flat_projection_delivers_row() {
    let result = run(
        r##"[
          { watch: "public.flat", id: "w0", expect_init: { value: [ { id: "p1" } ] } }
        ]"##,
    );
    assert_all_pass(&result, "flat-control");
}

/// THE PROBE (§7.1/§12.2): the nested projection `.parents { id, kids: .children
/// { id, label } }` MUST deliver `p1` with a `kids` member carrying its two child
/// rows. If the member is dropped (absent) or emptied, the observed row diverges
/// from the exact-match expectation and names the drop — the §7/§12 nested-member
/// bug the `pool-removal-preserves-recorded-funding` ledger entry actually rests
/// on. (If this now PASSES, the wave-3 row-materialization unification at 320b767
/// fixed it and that ledger attribution is stale — reported separately.)
#[test]
fn nested_collection_member_present() {
    let result = run(
        r##"[
          { watch: "public.tree", id: "w1", expect_init: { value: [
              { id: "p1", kids: [ { id: "c1", label: "x" }, { id: "c2", label: "y" } ] }
          ] } }
        ]"##,
    );
    assert_all_pass(&result, "nested-member-probe");
}
