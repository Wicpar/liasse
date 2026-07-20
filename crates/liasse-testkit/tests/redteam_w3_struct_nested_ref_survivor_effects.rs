//! RED-TEAM WAVE 3 — re-challenge of the wave-2 struct-nested-`$ref` fix (80f14a4).
//!
//! Wave 2 made the runtime SEE struct-nested refs (`refwalk::ref_sites`) so their
//! §5.6 validity, §21.1 `restrict`/`cascade`, and §5.4 rekey now hold. But the fix
//! then FAILS CLOSED on every SURVIVING-ROW field effect: `cascade.rs::nested_scalar_policy`
//! maps `OnDelete::Clear` (`$on_delete: "none"`) and `OnDelete::Patch(..)` to
//! `DeletePolicy::Undecided`, and `nested_member_policy` maps every set-member
//! effect except `restrict` to `Undecided`. At plan time an `Undecided` edge whose
//! target is deleted becomes `DeleteError::DanglingUndecided` → a `DanglingRef`
//! REJECTION (cascade.rs `delete_rejection`).
//!
//! The result is a §21.1 VIOLATION: a package DECLARES a spec-valid `$on_delete`
//! policy — `none`/`= patch`/set-member `cascade` — the model gate ACCEPTS it (the
//! model's own §21.1 gate descends into structs, so the package loads), yet at
//! runtime the declared policy is REFUSED instead of applied. §21.1 (verbatim):
//!
//!   "none   clear this optional ref"
//!   "`none` is valid only for an optional ref and expands to a patch assigning
//!    `none` to that referencing field."
//!   "cascade   delete the containing row or set member"
//!
//! and §5.6 (verbatim): "none — clear this optional ref". The spec mandates the
//! target deletion SUCCEEDS with the ref cleared / member dropped / row patched;
//! the runtime rejects the whole transition. Every FINDING below FAILS against the
//! current impl; every paired CONTROL (identical policy at a TOP-LEVEL field, where
//! `resolve_policy`/`resolve_member_policy` run the real effect) PASSES — isolating
//! the defect to the struct-nested fail-closed backstop.
//!
//! Expectations are hand-derived from SPEC.md, never from observed behaviour. The
//! corpus already pins the top-level clear/patch/drop semantics these mirror
//! (tests/05-state-model/red/reinserted-key-does-not-recapture-refs.hjson,
//! tests/21-deletion-erasure/common/patch-on-delete-rewrites-surviving-row.hjson).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#![allow(clippy::doc_lazy_continuation)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<w3-struct-nested-survivor>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("w3-struct-nested-survivor"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult, name: &str) {
    let ok = result.steps.iter().all(|s| s.result.is_pass());
    if !ok {
        for step in &result.steps {
            println!(
                "  {name} step {} [{}] -> {:?} observed={:?}",
                step.index, step.action, step.result, step.observed
            );
        }
    }
    assert!(ok, "{name}: a step diverged from its SPEC-derived expectation (see dump)");
}

// ── FINDING 1 ────────────────────────────────────────────────────────────────
// §21.1/§5.6: `$on_delete: "none"` on a struct-nested OPTIONAL ref MUST clear the
// ref when the target is deleted, and the deletion MUST succeed. The runtime
// rejects the deletion (nested `Clear` → `Undecided` → DanglingRef).
#[test]
fn struct_nested_none_must_clear_ref_and_commit() {
    let text = r##"{
      format: 1
      name: struct-nested-none-clears
      suite: scenario
      spec: ["#deletion", "§21.1", "#refs", "§5.6"]
      package: {
        $liasse: 1
        $app: "t.w3.snnone@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: {
            $key: "id"
            id: "text"
            meta: { owner: { $ref: "/accounts", $optional: true, $on_delete: "none" } }
          }
          $mut: { del: ".accounts - @id" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { del: ".del" } }
            tasks: { $view: ".tasks { id, meta }" }
          }
        }
        $data: { accounts: { a1: {} }, tasks: { t1: { meta: { owner: "a1" } } } }
      }
      steps: [
        // §21.1 "none — clear this optional ref": the delete succeeds, the ref clears.
        { call: "public.accounts.del", args: { id: "a1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.tasks", id: "w1", expect_init: { value: [ { id: "t1", meta: { owner: "$absent" } } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "struct-nested-none-clears");
}

// CONTROL 1: the identical optional ref + `none` at TOP level DOES clear and commit.
#[test]
fn control_toplevel_none_clears_ref_and_commits() {
    let text = r##"{
      format: 1
      name: control-toplevel-none-clears
      suite: scenario
      spec: ["#deletion", "§21.1", "#refs", "§5.6"]
      package: {
        $liasse: 1
        $app: "t.w3.ctlnone@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: { $key: "id", id: "text", owner: { $ref: "/accounts", $optional: true, $on_delete: "none" } }
          $mut: { del: ".accounts - @id" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { del: ".del" } }
            tasks: { $view: ".tasks { id, owner }" }
          }
        }
        $data: { accounts: { a1: {} }, tasks: { t1: { owner: "a1" } } }
      }
      steps: [
        { call: "public.accounts.del", args: { id: "a1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.tasks", id: "w1", expect_init: { value: [ { id: "t1", owner: "$absent" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-toplevel-none-clears");
}

// ── FINDING 2 ────────────────────────────────────────────────────────────────
// §21.1: a struct-nested `= patch` MUST patch the surviving containing row when the
// target is deleted (delete succeeds). The runtime rejects (nested `Patch` →
// `Undecided`).
#[test]
fn struct_nested_patch_must_rewrite_surviving_row_and_commit() {
    let text = r##"{
      format: 1
      name: struct-nested-patch-rewrites
      suite: scenario
      spec: ["#deletion", "§21.1", "#refs", "§5.6"]
      package: {
        $liasse: 1
        $app: "t.w3.snpatch@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: {
            $key: "id"
            id: "text"
            status: "text = 'active'"
            meta: { owner: { $ref: "/accounts", $optional: true, $on_delete: "= { status: 'orphaned', meta: { owner: none } }" } }
          }
          $mut: { del: ".accounts - @id" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { del: ".del" } }
            tasks: { $view: ".tasks { id, status }" }
          }
        }
        $data: { accounts: { a1: {} }, tasks: { t1: { meta: { owner: "a1" } } } }
      }
      steps: [
        { call: "public.accounts.del", args: { id: "a1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.tasks", id: "w1", expect_init: { value: [ { id: "t1", status: "orphaned" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "struct-nested-patch-rewrites");
}

// ── FINDING 3 ────────────────────────────────────────────────────────────────
// §21.1: a struct-nested `$set` of `$ref` with member `cascade` MUST drop the
// deleted target from the set (delete succeeds, containing row survives). The
// runtime rejects (nested member effect → `Undecided`).
#[test]
fn struct_nested_set_ref_cascade_must_drop_member_and_commit() {
    let text = r##"{
      format: 1
      name: struct-nested-set-ref-cascade-drops
      suite: scenario
      spec: ["#deletion", "§21.1", "#refs", "§5.6"]
      package: {
        $liasse: 1
        $app: "t.w3.snsetref@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: {
            $key: "id"
            id: "text"
            meta: { owners: { $set: { $ref: "/accounts", $on_delete: "cascade" } } }
          }
          $mut: { del: ".accounts - @id" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { del: ".del" } }
            tasks: { $view: ".tasks { id, meta }" }
          }
        }
        $data: { accounts: { a1: {}, a2: {} }, tasks: { t1: { meta: { owners: ["a1", "a2"] } } } }
      }
      steps: [
        { call: "public.accounts.del", args: { id: "a1" }, expect: { outcome: ok, "...": true } }
        // §21.1 "cascade — delete the containing row or set member": only a1's
        // membership is dropped; the task and a2 survive.
        { watch: "public.tasks", id: "w1", expect_init: { value: [ { id: "t1", meta: { owners: { $unordered: ["a2"] } } } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "struct-nested-set-ref-cascade-drops");
}

// CONTROL 2: the identical `= patch` at TOP level DOES patch the surviving row.
#[test]
fn control_toplevel_patch_rewrites_surviving_row() {
    let text = r##"{
      format: 1
      name: control-toplevel-patch-rewrites
      suite: scenario
      spec: ["#deletion", "§21.1", "#refs", "§5.6"]
      package: {
        $liasse: 1
        $app: "t.w3.ctlpatch@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: {
            $key: "id"
            id: "text"
            status: "text = 'active'"
            owner: { $ref: "/accounts", $optional: true, $on_delete: "= { status: 'orphaned', owner: none }" }
          }
          $mut: { del: ".accounts - @id" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { del: ".del" } }
            tasks: { $view: ".tasks { id, status }" }
          }
        }
        $data: { accounts: { a1: {} }, tasks: { t1: { owner: "a1" } } }
      }
      steps: [
        { call: "public.accounts.del", args: { id: "a1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.tasks", id: "w1", expect_init: { value: [ { id: "t1", status: "orphaned" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-toplevel-patch-rewrites");
}

// CONTROL 3: the identical set-of-ref `cascade` at TOP level DOES drop the member.
#[test]
fn control_toplevel_set_ref_cascade_drops_member() {
    let text = r##"{
      format: 1
      name: control-toplevel-set-ref-cascade
      suite: scenario
      spec: ["#deletion", "§21.1", "#refs", "§5.6"]
      package: {
        $liasse: 1
        $app: "t.w3.ctlsetref@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: { $key: "id", id: "text", owners: { $set: { $ref: "/accounts", $on_delete: "cascade" } } }
          $mut: { del: ".accounts - @id" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { del: ".del" } }
            tasks: { $view: ".tasks { id, owners }" }
          }
        }
        $data: { accounts: { a1: {}, a2: {} }, tasks: { t1: { owners: ["a1", "a2"] } } }
      }
      steps: [
        { call: "public.accounts.del", args: { id: "a1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.tasks", id: "w1", expect_init: { value: [ { id: "t1", owners: { $unordered: ["a2"] } } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-toplevel-set-ref-cascade");
}
