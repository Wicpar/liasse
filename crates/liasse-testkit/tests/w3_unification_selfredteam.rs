//! SELF-RED-TEAM (wave-3 unification) — proving the ONE canonical complete
//! row-materialization (computed values folded + nested keyed collections
//! descended, §5.2/§5.4/§5.5) holds at ADJACENT EDGES beyond the reported findings.
//!
//! Each test is derived from SPEC.md + the seeded inputs alone (arithmetic and the
//! §21.1 patch rule), never from observed behaviour. They exercise the unification
//! where two of the fixed paths COMPOSE:
//!
//!   1. `check_over_two_deep_nested_computed_admits` — a `$check` reading a
//!      TWO-DEEP nested computed (`sum(.teams.doubled)` on a department that is
//!      itself nested under a company), plus a company-level `$check` aggregating
//!      its nested `departments`. Proves the deep computed fold (F12) and the
//!      nested-collection-visible check row (F13) unify RECURSIVELY, not just one
//!      level down; a view confirms the folded value.
//!   2. `on_delete_patch_reads_nested_aggregated_computed` — an `$on_delete = {…}`
//!      patch reading `.tally`, a computed that aggregates a NESTED collection's
//!      computed field (`sum(.lines.double)`). Proves the patch's `.` cell is the
//!      COMPLETE row (F14 computed fold ∧ F12 nested descent compose), so a patch
//!      resolves a value that only exists once nested computed is folded.
//!   3. `struct_nested_none_clear_preserves_sibling_struct_field` — a struct-nested
//!      optional ref `$on_delete: "none"` whose containing struct ALSO carries a
//!      plain sibling field. Proves the F3 nested-clear rebuild is SURGICAL: it
//!      clears only the leaf ref and preserves the sibling (`meta.label`), beyond
//!      the reported single-field-struct case.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
#![allow(clippy::doc_lazy_continuation)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<w3-unify-selfredteam>"), &BTreeSet::new()).expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("w3-unify-selfredteam"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult, name: &str) {
    let ok = result.steps.iter().all(|s| s.result.is_pass());
    if !ok {
        for step in &result.steps {
            println!("  {name} step {} [{}] -> {:?} observed={:?}", step.index, step.action, step.result, step.observed);
        }
    }
    assert!(ok, "{name}: a step diverged from its SPEC-derived expectation (see dump)");
}

// ── 1. A `$check` reading a TWO-DEEP nested computed must admit ────────────────
// companies → departments → teams. `teams.doubled = size*2` (leaf computed),
// `departments.dept_total = sum(.teams.doubled)` (a computed aggregating the
// deeper nested computed). A department-level `$check` reads `sum(.teams.doubled)`
// and a company-level `$check` aggregates `.departments`. Genesis finalizes every
// seeded row, so both checks RUN and must see the nested tree; if the deep fold or
// the nested-collection-visible check row were missing, they would fault and the
// package would fail to load. Oracle: size 3,4 → doubled 6,8 → dept_total = 14.
#[test]
fn check_over_two_deep_nested_computed_admits() {
    let text = r##"{
      format: 1
      name: check-two-deep-nested-computed
      suite: scenario
      spec: ["#state-model", "§5.2", "§5.4", "#mutations", "§8.8"]
      package: {
        $liasse: 1
        $app: "t.w3sr.deepcheck@1.0.0"
        $model: {
          companies: {
            $key: "id"
            id: "text"
            departments: {
              $key: "did"
              did: "text"
              teams: { $key: "tid", tid: "text", size: "int", doubled: "= size * 2" }
              dept_total: "= sum(.teams.doubled)"
              $check: ["sum(.teams.doubled) >= 0"]
            }
            $check: ["count(.departments) >= 0"]
          }
          $public: {
            deps: { $view: ".companies[:c].departments[:d] { comp: c.id, dep: d.did, dept_total: d.dept_total }" }
          }
        }
        $data: { companies: { c1: { departments: { d1: { teams: { t1: { size: "3" }, t2: { size: "4" } } } } } } }
      }
      steps: [
        { watch: "public.deps", id: "w1", expect_init: { value: [ { comp: "c1", dep: "d1", dept_total: "14" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "check-two-deep-nested-computed");
}

// ── 2. An `$on_delete` patch reading a nested-aggregated computed ──────────────
// The invoice's `tally = sum(.lines.double)` aggregates a NESTED collection whose
// `double = amount*2` is itself computed. The `$on_delete` patch `.` binds to the
// referencing invoice, so `.tally` must resolve to the fully-folded value — which
// exists only when the patch's `.` cell descends `.lines` AND folds their computed.
// Oracle: amounts 5,10 → double 10,20 → tally = 30 → note patched to 30.
#[test]
fn on_delete_patch_reads_nested_aggregated_computed() {
    let text = r##"{
      format: 1
      name: on-delete-patch-nested-aggregated-computed
      suite: scenario
      spec: ["#deletion", "§21.1", "#state-model", "§5.2", "§5.4"]
      package: {
        $liasse: 1
        $app: "t.w3sr.odnested@1.0.0"
        $model: {
          projects: { $key: "id", id: "text" }
          invoices: {
            $key: "id"
            id: "text"
            lines: { $key: "lid", lid: "text", amount: "int", double: "= amount * 2" }
            tally: "= sum(.lines.double)"
            note: "int?"
            project: {
              $ref: "/projects"
              $optional: true
              $on_delete: "= { project: none, note: .tally }"
            }
          }
          $mut: { del: ".projects - @id" }
          inv_view: { $view: ".invoices { id, tally, note, project, $sort: [id] }" }
          $public: {
            projects: { $view: ".projects { id }", $mut: { delete: ".del" } }
            invoices: { $view: ".inv_view" }
          }
        }
        $data: {
          projects: { p1: {} }
          invoices: { i1: { project: "p1", lines: { l1: { amount: "5" }, l2: { amount: "10" } } } }
        }
      }
      steps: [
        // §5.2/§5.4: tally = sum(line doubles) = 10 + 20 = 30, visible pre-delete.
        { watch: "public.invoices", id: "w1", expect_init: { value: [
          { id: "i1", tally: "30", note: "$absent", project: "p1" }
        ] } }
        { call: "public.projects.delete", args: { id: "p1" }, expect: { outcome: ok, "...": true } }
        // §21.1: `.` is the invoice; `.tally` = 30 -> note = 30, project cleared.
        { expect_view: { watch: "w1", value: [
          { id: "i1", tally: "30", note: "30", project: "$absent" }
        ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "on-delete-patch-nested-aggregated-computed");
}

// ── 3. A struct-nested `none` clear preserves the struct's sibling fields ──────
// The struct `meta` carries both a plain `label` and the optional ref `owner`
// (`$on_delete: "none"`). Deleting the account MUST clear only `meta.owner` and
// leave `meta.label` intact (§21.1 "none — clear this optional ref"): the nested
// rebuild is surgical, not a wholesale struct replacement.
#[test]
fn struct_nested_none_clear_preserves_sibling_struct_field() {
    let text = r##"{
      format: 1
      name: struct-nested-none-preserves-sibling
      suite: scenario
      spec: ["#deletion", "§21.1", "#refs", "§5.6", "#state-model", "§5.3"]
      package: {
        $liasse: 1
        $app: "t.w3sr.snsibling@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: {
            $key: "id"
            id: "text"
            meta: { label: "text", owner: { $ref: "/accounts", $optional: true, $on_delete: "none" } }
          }
          $mut: { del: ".accounts - @id" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { del: ".del" } }
            tasks: { $view: ".tasks { id, meta }" }
          }
        }
        $data: { accounts: { a1: {} }, tasks: { t1: { meta: { label: "keep-me", owner: "a1" } } } }
      }
      steps: [
        { call: "public.accounts.del", args: { id: "a1" }, expect: { outcome: ok, "...": true } }
        // §21.1: owner cleared, label preserved — the rebuild touched only the leaf.
        { watch: "public.tasks", id: "w1", expect_init: { value: [
          { id: "t1", meta: { label: "keep-me", owner: "$absent" } }
        ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "struct-nested-none-preserves-sibling");
}
