//! RED-TEAM finding (§21.1 / §22.1): an `$on_delete: = { … }` patch that assigns
//! a **reference** field a value pointing at a non-existent target commits a
//! DANGLING REFERENCE instead of being rejected.
//!
//! §21.1 (deletion): "The complete plan then applies atomically, updates refs and
//! indexes, and **checks every resulting constraint**." and "A failing
//! restriction, cascade, patch, check, or **other state constraint** rejects the
//! entire transition." §22.1 lists "reference validity and delete policy" among
//! the state constraints that hold in EVERY committed state. So a surviving row
//! whose `$on_delete` patch leaves a ref pointing at no live target is an invalid
//! committed state and the whole deletion MUST be rejected.
//!
//! Observed divergence: the deletion COMMITS and the referencing row keeps a
//! dangling ref (its ref field holds a bare scalar key that resolves to nothing).
//!
//! Root cause (hand-traced):
//!   * `liasse-runtime/src/interp.rs::apply_deletion` applies each `= patch`
//!     assignment verbatim and runs only `rules::normalize_field` — it never runs
//!     the ref coercion (`rules::coerce_fields`/`coerce_value`) the ordinary write
//!     path applies, so a ref-typed patched field keeps a bare `Value::Text`
//!     rather than a `Value::Ref`.
//!   * `liasse-runtime/src/rules.rs::check_refs` (the `Some(_) => {}` arm, ~L671)
//!     validates a ref field ONLY when it holds a `Value::Ref`; a ref field
//!     holding any other non-`None` value is silently accepted, so the bare
//!     scalar escapes the dangling-ref check that `finalize` otherwise runs.
//!
//! The three controls below isolate the defect precisely:
//!   * `control_ordinary_bad_ref_assignment_rejected` — the ordinary write path
//!     DOES reject `team = 'ghost'` (refs are validated there), so the refs check
//!     exists and works.
//!   * `control_on_delete_patch_check_is_rerun` — an `$on_delete` patch that
//!     violates a row `$check` IS rejected, so the post-patch constraint pipeline
//!     genuinely re-runs — the omission is ref-specific, not a missing pipeline.
//!   * `control_on_delete_patch_to_existing_ref_commits` — the SAME patch to an
//!     EXISTING team commits cleanly, so the patch mechanism itself is sound.
//!   * `control_on_delete_patch_uniqueness_is_rerun` — an `$on_delete` patch that
//!     violates `$unique` IS rejected, confirming other resulting constraints are
//!     re-checked and the gap is confined to reference validity.
//!
//! Every expectation is derived from SPEC.md text alone (§21.1, §22.1, §5.6),
//! never from observed engine behaviour.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<on-delete-dangling-ref>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("on-delete-dangling-ref"), SuiteKind::Red, &case)
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

// ── THE FINDING ────────────────────────────────────────────────────────────
// §21.1/§22.1: an `$on_delete = { team: 'ghost' }` patch repoints the surviving
// task's REQUIRED ref to a non-existent team. The post-delete state has a
// dangling reference, so the whole deletion MUST be rejected. This test FAILS
// against the current impl: the deletion commits and `team` becomes a dangling
// 'ghost' ref.
#[test]
fn on_delete_patch_creating_dangling_ref_must_reject() {
    let text = r##"{
      format: 1
      name: on-delete-patch-dangling-ref
      suite: scenario
      spec: ["#deletion", "§21.1", "#refs", "§5.6", "§22.1"]
      package: {
        $liasse: 1
        $app: "t.dangling@1.0.0"
        $model: {
          teams: { $key: "id", id: "text" }
          projects: { $key: "id", id: "text", name: "text" }
          tasks: {
            $key: "id"
            id: "text"
            team: { $ref: "/teams" }
            project: {
              $ref: "/projects"
              $optional: true
              // Repoints the REQUIRED `team` ref to a non-existent 'ghost' team.
              $on_delete: "= { project: none, team: 'ghost' }"
            }
          }
          $mut: { delete_project: ".projects - @id" }
          tv: { $view: ".tasks { id, team, project, $sort: [id] }" }
          $public: {
            tasks: { $view: ".tv" }
            projects: { $view: ".projects { id }", $mut: { delete: ".delete_project" } }
          }
        }
        $data: {
          teams: { tm1: {} }
          projects: { p1: { name: "Apollo" } }
          tasks: { t1: { team: "tm1", project: "p1" } }
        }
      }
      steps: [
        // §21.1: the resulting dangling ref is an invalid committed state -> reject.
        { call: "public.projects.delete", args: { id: "p1" },
          expect: { outcome: rejected, violates: ["#deletion", "§21.1"] } }
        // The rejection leaves live state exactly as it was (§21.1 atomicity).
        { watch: "public.tasks", id: "w1",
          expect_init: { value: [ { id: "t1", team: "tm1", project: "p1" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "on-delete-patch-dangling-ref");
}

// ── CONTROL: ordinary write path DOES validate refs ──────────────────────────
#[test]
fn control_ordinary_bad_ref_assignment_rejected() {
    let text = r##"{
      format: 1
      name: control-ordinary-bad-ref
      suite: scenario
      spec: ["#refs", "§5.6", "§22.1"]
      package: {
        $liasse: 1
        $app: "t.ctlref@1.0.0"
        $model: {
          teams: { $key: "id", id: "text" }
          tasks: { $key: "id", id: "text", team: { $ref: "/teams" } }
          $mut: { set_team: ".tasks[@id].team = @team" }
          tv: { $view: ".tasks { id, team, $sort: [id] }" }
          $public: { tasks: { $view: ".tv", $mut: { set_team: ".set_team" } } }
        }
        $data: { teams: { tm1: {} }, tasks: { t1: { team: "tm1" } } }
      }
      steps: [
        { call: "public.tasks.set_team", args: { id: "t1", team: "ghost" },
          expect: { outcome: rejected, violates: ["#refs", "§5.6"] } }
        { watch: "public.tasks", id: "w1", expect_init: { value: [ { id: "t1", team: "tm1" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-ordinary-bad-ref");
}

// ── CONTROL: a `$check` violated by the patch IS re-run (post-patch pipeline runs)
#[test]
fn control_on_delete_patch_check_is_rerun() {
    let text = r##"{
      format: 1
      name: control-patch-check
      suite: scenario
      spec: ["#deletion", "§21.1"]
      package: {
        $liasse: 1
        $app: "t.ctlchk@1.0.0"
        $model: {
          projects: { $key: "id", id: "text", name: "text" }
          tasks: {
            $key: "id"
            id: "text"
            status: "text = 'live'"
            $check: [".status != 'orphan'", "status must not be orphan"]
            project: { $ref: "/projects", $optional: true, $on_delete: "= { status: 'orphan' }" }
          }
          $mut: { delete_project: ".projects - @id" }
          tv: { $view: ".tasks { id, status, project, $sort: [id] }" }
          $public: {
            tasks: { $view: ".tv" }
            projects: { $view: ".projects { id }", $mut: { delete: ".delete_project" } }
          }
        }
        $data: { projects: { p1: { name: "Apollo" } }, tasks: { t1: { project: "p1" } } }
      }
      steps: [
        { call: "public.projects.delete", args: { id: "p1" },
          expect: { outcome: rejected, violates: ["#deletion", "§21.1"] } }
        { watch: "public.tasks", id: "w1",
          expect_init: { value: [ { id: "t1", status: "live", project: "p1" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-patch-check");
}

// ── CONTROL: a `$unique` violated by the patch IS re-run (gap is ref-specific)
#[test]
fn control_on_delete_patch_uniqueness_is_rerun() {
    let text = r##"{
      format: 1
      name: control-patch-unique
      suite: scenario
      spec: ["#deletion", "§21.1", "§5.7"]
      package: {
        $liasse: 1
        $app: "t.ctluniq@1.0.0"
        $model: {
          projects: { $key: "id", id: "text", name: "text" }
          tasks: {
            $key: "id"
            id: "text"
            slug: "text"
            $unique: ["slug"]
            // Patch drives t1.slug to "dup", colliding with t2.slug.
            project: { $ref: "/projects", $optional: true, $on_delete: "= { slug: 'dup' }" }
          }
          $mut: { delete_project: ".projects - @id" }
          tv: { $view: ".tasks { id, slug, $sort: [id] }" }
          $public: {
            tasks: { $view: ".tv" }
            projects: { $view: ".projects { id }", $mut: { delete: ".delete_project" } }
          }
        }
        $data: {
          projects: { p1: { name: "Apollo" } }
          tasks: { t1: { slug: "a", project: "p1" }, t2: { slug: "dup" } }
        }
      }
      steps: [
        { call: "public.projects.delete", args: { id: "p1" },
          expect: { outcome: rejected, violates: ["#deletion", "§21.1"] } }
        { watch: "public.tasks", id: "w1",
          expect_init: { value: [ { id: "t1", slug: "a" }, { id: "t2", slug: "dup" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-patch-unique");
}

// ── CONTROL: the same patch machinery to an EXISTING ref commits cleanly ──────
#[test]
fn control_on_delete_patch_to_existing_ref_commits() {
    let text = r##"{
      format: 1
      name: control-patch-existing-ref
      suite: scenario
      spec: ["#deletion", "§21.1"]
      package: {
        $liasse: 1
        $app: "t.ctlok@1.0.0"
        $model: {
          teams: { $key: "id", id: "text" }
          projects: { $key: "id", id: "text", name: "text" }
          tasks: {
            $key: "id"
            id: "text"
            team: { $ref: "/teams" }
            project: { $ref: "/projects", $optional: true, $on_delete: "= { project: none, team: 'tm2' }" }
          }
          $mut: { delete_project: ".projects - @id" }
          tv: { $view: ".tasks { id, team, project, $sort: [id] }" }
          $public: {
            tasks: { $view: ".tv" }
            projects: { $view: ".projects { id }", $mut: { delete: ".delete_project" } }
          }
        }
        $data: {
          teams: { tm1: {}, tm2: {} }
          projects: { p1: { name: "Apollo" } }
          tasks: { t1: { team: "tm1", project: "p1" } }
        }
      }
      steps: [
        { call: "public.projects.delete", args: { id: "p1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.tasks", id: "w1",
          expect_init: { value: [ { id: "t1", team: "tm2", project: "$absent" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-patch-existing-ref");
}
