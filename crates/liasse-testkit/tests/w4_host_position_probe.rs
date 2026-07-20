//! RED-TEAM WAVE 4 (fresh-deep) — §16.5 execution-context enforcement over the
//! positions the model checker only `parse_only`s and the runtime compiles.
//!
//! §16.5: "A call to a `$requires`-registered namespace (§16.2) is legal only
//! within a mutation program — the atomic sequential program of Mutations,
//! including auth mutations (§11.5), delete patches (§21.1), and migration delta
//! programs (§20.1)." So an `$on_delete` `= {…}` patch (a delete-time mutation
//! program) MAY call a registered namespace, exactly like a `$mut` body.
//!
//! Expectations are derived from SPEC.md + the deterministic sim namespace op
//! (`double`: (int) -> int, x -> 2x): the CONTROL (a pure app call inside a `$mut`
//! body) and the PROBE (the SAME call inside an `$on_delete` patch) must both load
//! and evaluate.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
#![allow(clippy::doc_lazy_continuation)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<w4-hostpos>"), &BTreeSet::new()).expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("w4-hostpos"), SuiteKind::Red, &case)
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

// ── CONTROL: a pure app-namespace call inside a `$mut` body loads and evaluates ─
// Proves the sim namespace is registered/resolved and a `u.f(int) -> int` call
// type-checks and runs in the one indisputable mutation position. `double(3) = 6`.
#[test]
fn control_app_namespace_call_in_mut_body() {
    let text = r##"{
      format: 1
      name: control-app-call-in-mut
      suite: scenario
      spec: ["#host-namespaces", "§16.5"]
      package: {
        $liasse: 1
        $app: "t.w4.ctlmut@1.0.0"
        $requires: { u: "test.util@1" }
        $model: {
          rows: { $key: "id", id: "text", base: "int", tag: "int?" }
          $mut: { tagit: [ ".rows[@id].tag = u.f(.rows[@id].base)" ] }
          $public: {
            rows: { $view: ".rows { id, base, tag, $sort: [id] }", $mut: { tagit: ".tagit" } }
          }
        }
        $data: { rows: { r1: { base: "3" } } }
      }
      hosts: {
        namespaces: [
          { id: "test.util", version: "1.0.0", interface_hash: "ih-util-double",
            functions: { f: { signature: "(int) -> int", effect: "pure", op: "double" } } }
        ]
      }
      steps: [
        { call: "public.rows.tagit", args: { id: "r1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.rows", id: "w1", expect_init: { value: [ { id: "r1", base: "3", tag: "6" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-app-call-in-mut");
}

// ── CONTROL 2: an `$on_delete` patch WITHOUT an app call loads and evaluates ────
// Establishes the on_delete patch machinery works for a plain expression, so the
// only variable in the PROBE below is the app-namespace call.
#[test]
fn control_on_delete_patch_without_app_call() {
    let text = r##"{
      format: 1
      name: control-on-delete-plain
      suite: scenario
      spec: ["#deletion", "§21.1"]
      package: {
        $liasse: 1
        $app: "t.w4.ctlod@1.0.0"
        $model: {
          projects: { $key: "id", id: "text" }
          invoices: {
            $key: "id"
            id: "text"
            base: "int"
            tag: "int?"
            project: { $ref: "/projects", $optional: true, $on_delete: "= { project: none, tag: .base }" }
          }
          $mut: { del: ".projects - @id" }
          $public: {
            projects: { $view: ".projects { id }", $mut: { delete: ".del" } }
            invoices: { $view: ".invoices { id, base, tag, project, $sort: [id] }" }
          }
        }
        $data: { projects: { p1: {} }, invoices: { i1: { base: "3", project: "p1" } } }
      }
      steps: [
        { call: "public.projects.delete", args: { id: "p1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.invoices", id: "w1",
          expect_init: { value: [ { id: "i1", base: "3", tag: "3", project: "$absent" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-on-delete-plain");
}

// ── PROBE: a pure app-namespace call INSIDE an `$on_delete` patch (§16.5) ───────
// §16.5 lists "delete patches (§21.1)" among the mutation-program positions where a
// `$requires`-registered namespace call is LEGAL. The patch `= { project: none,
// tag: u.f(.base) }` must therefore load and, on delete of the referenced project,
// evaluate `u.f(3) = double(3) = 6` into the invoice's `tag`. If the runtime rejects
// this at load — the `compile_on_delete` scope never threads the host ops even though
// it correctly marks the position `HostPosition::Mutation` — the package fails to load
// and this asserts the §16.5/§21.1 false-rejection.
#[test]
fn app_namespace_call_in_on_delete_patch() {
    let text = r##"{
      format: 1
      name: app-call-in-on-delete
      suite: scenario
      spec: ["#host-namespaces", "§16.5", "#deletion", "§21.1"]
      package: {
        $liasse: 1
        $app: "t.w4.odapp@1.0.0"
        $requires: { u: "test.util@1" }
        $model: {
          projects: { $key: "id", id: "text" }
          invoices: {
            $key: "id"
            id: "text"
            base: "int"
            tag: "int?"
            project: { $ref: "/projects", $optional: true, $on_delete: "= { project: none, tag: u.f(.base) }" }
          }
          $mut: { del: ".projects - @id" }
          $public: {
            projects: { $view: ".projects { id }", $mut: { delete: ".del" } }
            invoices: { $view: ".invoices { id, base, tag, project, $sort: [id] }" }
          }
        }
        $data: { projects: { p1: {} }, invoices: { i1: { base: "3", project: "p1" } } }
      }
      hosts: {
        namespaces: [
          { id: "test.util", version: "1.0.0", interface_hash: "ih-util-double",
            functions: { f: { signature: "(int) -> int", effect: "pure", op: "double" } } }
        ]
      }
      steps: [
        { call: "public.projects.delete", args: { id: "p1" }, expect: { outcome: ok, "...": true } }
        // §16.5/§21.1: u.f(3) = 6 patched into tag; project cleared.
        { watch: "public.invoices", id: "w1",
          expect_init: { value: [ { id: "i1", base: "3", tag: "6", project: "$absent" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "app-call-in-on-delete");
}
