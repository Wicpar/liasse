//! RED-TEAM probe: export/restore of a top-level collection whose §5.3 static
//! struct carries a NON-scalar member — a `set` — and an empty-set edge.
//!
//! The portable decode-type builder (`singleton::optional_decode_struct` /
//! `optionalize`) recursively optional-wraps a struct member's OWN members, but
//! only special-cases `Type::Struct`; every other member type (a `set`, here) is
//! wrapped as `Optional(Set)` and left to the shared `Type::decode`. This closes
//! the gap the existing static-struct/round-trip cases leave: those carry only
//! scalar/optional struct members, never a set INSIDE a static struct. A codec
//! that mishandled a set nested in a struct (dropping members, faulting the
//! optional-wrapped set, or losing set canonical order) would fail restore.
//!
//! Externally deducible: §5.5 (a set reads in element-type canonical order;
//! duplicates collapse), §19.10/§19.2 (restore reproduces the same owned state),
//! §5.3 (a static struct shares its row's lifecycle). The seed is the known input;
//! the restored view must reproduce the canonicalized set exactly.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

const APP: &str = r##"{
  format: 1
  name: static-struct-set-export-restore-probe
  suite: scenario
  spec: ["#history", "§19.10", "§19.2", "§5.3", "§5.5"]
  package: {
    $liasse: 1
    $app: "t.hist.sstructset@1.0.0"
    $model: {
      docs: {
        $key: "id"
        id: "text"
        meta: {
          title: "text"
          tags: { $set: "text" }
        }
      }
      $public: {
        docs: { $view: ".docs { id, meta, $sort: [id] }" }
      }
    }
    $data: {
      docs: {
        d1: { meta: { title: "hello", tags: ["b", "a", "c"] } }
        d2: { meta: { title: "world", tags: [] } }
      }
    }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let allowed: BTreeSet<String> =
        ["export", "in_sandbox", "restore"].into_iter().map(String::from).collect();
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<static-struct-set-export-restore-probe>"), &allowed)
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("19-history-artifacts"), SuiteKind::Red, &case)
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

/// PASSING CONTROL: the live view reproduces the set inside the static struct in
/// canonical ascending order, and the empty set as an empty array.
#[test]
fn live_view_reproduces_static_struct_set() {
    let result = run(
        r##"[
          { watch: "public.docs", id: "w1", expect_init: { value: [
            { id: "d1", meta: { title: "hello", tags: ["a", "b", "c"] } }
            { id: "d2", meta: { title: "world", tags: [] } }
          ] } }
        ]"##,
    );
    assert_all_pass(&result);
}

/// THE PROBE: export then restore into a fresh sandbox, and assert the set nested
/// in the static struct round-trips exactly — canonical order preserved, the empty
/// set restored as empty.
#[test]
fn static_struct_set_survives_export_restore() {
    let result = run(
        r##"[
          { export: { as: "a1" }, expect: { outcome: ok } }
          { in_sandbox: "s1", steps: [
            { restore: { from: "a1" }, expect: { outcome: ok } }
            { watch: "public.docs", id: "wd", expect_init: { value: [
              { id: "d1", meta: { title: "hello", tags: ["a", "b", "c"] } }
              { id: "d2", meta: { title: "world", tags: [] } }
            ] } }
          ] }
        ]"##,
    );
    assert_all_pass(&result);
}
