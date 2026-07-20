//! RED-TEAM WAVE 4 (fresh-deep) — nested-subtree removal on delete + edge crashes,
//! over the UNIFIED row materialization (commit 320b767).
//!
//! §5.4/§5.5: a nested keyed collection is real row state living under its parent
//! row's identity. §21.1/§8.5: deleting a row removes the row; its nested keyed
//! children share the row's identity and lifecycle (§5.3 for structs; §5.4 nests
//! keyed children under the parent), so they cannot outlive the parent. If a
//! parent delete leaves nested rows in durable state, a later row re-inserted at
//! the SAME key resurrects the orphans — an observable state-integrity violation.
//!
//! The sharpest SPEC hook is §5.5: "When a containing row or struct is created, an
//! omitted child set or keyed collection starts empty." A re-inserted parent that
//! OMITS its nested collection MUST expose it empty in every committed state; a
//! resurrected orphan makes it non-empty with no insert — a direct §5.5 violation.
//!
//! Every expectation is derived from SPEC.md + the inputs alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
#![allow(clippy::doc_lazy_continuation)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<w4-orphan>"), &BTreeSet::new()).expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("w4-orphan"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult, name: &str) {
    let ok = result.verdict.is_pass();
    if !ok {
        println!("== {name}: verdict={:?}", result.verdict);
        for step in &result.steps {
            println!("  step {} [{}] -> {:?} observed={:?}", step.index, step.action, step.result, step.observed);
        }
    }
    assert!(ok, "{name}: diverged from SPEC-derived expectation (see dump)");
}

// ── ORPHAN 1: delete a parent, re-insert the SAME key — nested rows must NOT
//    resurrect ──────────────────────────────────────────────────────────────────
// company c1 seeded with departments d1,d2. Deleting c1 must remove its whole nested
// subtree (§5.4/§21.1). Re-inserting a fresh c1 (no departments) must therefore show
// an EMPTY department set; if the old d1,d2 reappear, the parent delete orphaned them.
#[test]
fn parent_delete_then_reinsert_no_orphan_resurrection() {
    let text = r##"{
      format: 1
      name: parent-delete-reinsert-no-orphan
      suite: scenario
      spec: ["#state-model", "§5.5", "§5.4", "#deletion", "§21.1"]
      package: {
        $liasse: 1
        $app: "t.w4.orphan1@1.0.0"
        $model: {
          companies: { $key: "id", id: "text", departments: { $key: "did", did: "text" } }
          $mut: { del: ".companies - @id", add: ".companies + { id: @id }" }
          $public: {
            companies: { $view: ".companies { id, $sort: [id] }", $mut: { del: ".del", add: ".add" } }
            deps: { $view: ".companies[:c].departments[:d] { comp: c.id, dep: d.did, $sort: [comp, dep] }" }
          }
        }
        $data: { companies: { c1: { departments: { d1: {}, d2: {} } } } }
      }
      steps: [
        { watch: "public.deps", id: "w1",
          expect_init: { value: [ { comp: "c1", dep: "d1" }, { comp: "c1", dep: "d2" } ] } }
        { call: "public.companies.del", args: { id: "c1" }, expect: { outcome: ok, "...": true } }
        // Parent gone: no departments materialize.
        { expect_view: { watch: "w1", value: [] } }
        { call: "public.companies.add", args: { id: "c1" }, expect: { outcome: ok, "...": true } }
        // §5.4/§21.1: the fresh c1 has NO departments. Orphans must not resurrect.
        { expect_view: { watch: "w1", value: [] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "parent-delete-reinsert-no-orphan");
}

// ── ORPHAN 2: delete a parent, then RESTART — durable state must carry no orphan ─
// Same as ORPHAN 1 but forces a durable round-trip: after deleting c1 and
// re-inserting it, a restart re-gathers committed state from the store. An orphaned
// nested row that survived the parent delete would re-materialize under the fresh c1
// after restart.
#[test]
fn parent_delete_reinsert_survives_restart_without_orphan() {
    let text = r##"{
      format: 1
      name: parent-delete-reinsert-restart-no-orphan
      suite: scenario
      spec: ["#state-model", "§5.5", "§5.4", "#deletion", "§21.1", "#runtime", "§22.2"]
      package: {
        $liasse: 1
        $app: "t.w4.orphan2@1.0.0"
        $model: {
          companies: { $key: "id", id: "text", departments: { $key: "did", did: "text" } }
          $mut: { del: ".companies - @id", add: ".companies + { id: @id }" }
          $public: {
            companies: { $view: ".companies { id, $sort: [id] }", $mut: { del: ".del", add: ".add" } }
            deps: { $view: ".companies[:c].departments[:d] { comp: c.id, dep: d.did, $sort: [comp, dep] }" }
          }
        }
        $data: { companies: { c1: { departments: { d1: {}, d2: {} } } } }
      }
      steps: [
        { call: "public.companies.del", args: { id: "c1" }, expect: { outcome: ok, "...": true } }
        { call: "public.companies.add", args: { id: "c1" }, expect: { outcome: ok, "...": true } }
        { restart: {} }
        { watch: "public.deps", id: "w1", expect_init: { value: [] } }
        { watch: "public.companies", id: "w2", expect_init: { value: [ { id: "c1" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "parent-delete-reinsert-restart-no-orphan");
}

// ── ORPHAN 3: overwrite a parent via key-delete then bulk re-add other parents ──
// A different resurrection path: delete c1 (with nested), keep c2 (with nested),
// confirm only c2's nested rows remain — the delete removed ONLY c1's subtree, not
// c2's, and left no c1 orphan.
#[test]
fn parent_delete_removes_only_its_own_nested_subtree() {
    let text = r##"{
      format: 1
      name: parent-delete-only-own-subtree
      suite: scenario
      spec: ["#state-model", "§5.5", "§5.4", "#deletion", "§21.1"]
      package: {
        $liasse: 1
        $app: "t.w4.orphan3@1.0.0"
        $model: {
          companies: { $key: "id", id: "text", departments: { $key: "did", did: "text" } }
          $mut: { del: ".companies - @id" }
          $public: {
            companies: { $view: ".companies { id, $sort: [id] }", $mut: { del: ".del" } }
            deps: { $view: ".companies[:c].departments[:d] { comp: c.id, dep: d.did, $sort: [comp, dep] }" }
          }
        }
        $data: {
          companies: {
            c1: { departments: { d1: {}, d2: {} } }
            c2: { departments: { e1: {} } }
          }
        }
      }
      steps: [
        { call: "public.companies.del", args: { id: "c1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.deps", id: "w1", expect_init: { value: [ { comp: "c2", dep: "e1" } ] } }
        { restart: {} }
        { watch: "public.deps", id: "w2", expect_init: { value: [ { comp: "c2", dep: "e1" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "parent-delete-only-own-subtree");
}
