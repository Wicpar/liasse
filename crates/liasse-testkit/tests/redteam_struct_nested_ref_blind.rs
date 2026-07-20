//! RED-TEAM finding (WAVE 2) — §5.6 / §22.1 / §21.1 / §5.4:
//! the runtime is BLIND to references declared inside a static struct.
//!
//! A `$ref` is a legal member of a static struct (§5.3: "Structs MAY contain
//! fields, structs, sets, views, and nested keyed collections."; a ref is a field
//! kind — §5.1/§5.6). It compiles correctly: `compiled.rs::compile_struct` builds
//! a `CompiledField { reference: Some(RefInfo { .. }) }` and stores it in the
//! struct's `fields`. But EVERY runtime pass that reasons about references only
//! iterates the collection's TOP-LEVEL `collection.fields` and never descends into
//! `collection.structs[].fields`, so a struct-nested ref escapes all four
//! reference invariants:
//!
//!   1. `rules.rs::check_refs` (~L651) — reference validity (§5.6/§22.1: a ref
//!      "MUST resolve to an existing row"; §22.1 lists "reference validity and
//!      delete policy" among the state constraints that hold in EVERY committed
//!      state). A dangling struct-nested ref is silently accepted at insert.
//!   2. `cascade.rs::plan` (~L64-114) — deletion planning under `$on_delete:
//!      restrict` (§21.1: "restrict — reject deletion while the ref exists").
//!      Deleting the target succeeds even though a surviving struct-nested ref
//!      points at it.
//!   3. `cascade.rs::plan` — deletion planning under `$on_delete: cascade` (§21.1:
//!      "cascade — delete the containing row or set member"). Deleting the target
//!      leaves the containing row alive with a dangling struct-nested ref.
//!   4. `interp.rs::rewrite_inbound_refs_across` (~L1992) — atomic rekey (§5.4:
//!      "The runtime MUST update every ref that targets the row ... in the same
//!      transition."). Rekeying the target leaves the struct-nested ref showing the
//!      stale prior key.
//!
//! The bug is aggravated by a MODEL/RUNTIME ASYMMETRY: the CORE model's §21.1
//! deferred-delete gate (`liasse-model/src/delete.rs::collect_refs`) DOES descend
//! into `Node::Struct`, so the developer is FORCED to declare a policy
//! (`cascade`/`restrict`) on a struct-nested ref and the package loads — then the
//! runtime silently ignores that very policy. See the companion model-layer control
//! `redteam_struct_nested_ref_gate_asymmetry.rs`.
//!
//! Each finding below FAILS against the current implementation; every paired
//! control (the identical ref hoisted to a TOP-LEVEL field) PASSES, isolating the
//! defect to the struct-descent gap. Expectations are hand-derived from SPEC.md,
//! never from observed behaviour.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<struct-nested-ref-blind>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("struct-nested-ref-blind"), SuiteKind::Red, &case)
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
// §5.6/§22.1: a struct-nested `$ref` pointing at a non-existent target is a
// dangling reference — an invalid committed state — yet the insert COMMITS.
#[test]
fn struct_nested_ref_dangling_on_insert_must_reject() {
    let text = r##"{
      format: 1
      name: struct-nested-ref-dangling-insert
      suite: scenario
      spec: ["#refs", "§5.6", "#runtime", "§22.1"]
      package: {
        $liasse: 1
        $app: "t.snrb.insert@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: {
            $key: "id"
            id: "text"
            meta: { owner: { $ref: "/accounts" } }
          }
          $mut: { add: ".tasks + { id: @id, meta: { owner: @owner } }" }
          $public: { tasks: { $view: ".tasks { id }", $mut: { add: ".add" } } }
        }
        $data: { accounts: { a1: {} } }
      }
      steps: [
        { call: "public.tasks.add", args: { id: "t1", owner: "ghost" },
          expect: { outcome: rejected, violates: ["#refs", "§5.6"] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "struct-nested-ref-dangling-insert");
}

// CONTROL 1: the identical ref as a TOP-LEVEL field IS validated (rejects).
#[test]
fn control_toplevel_ref_dangling_on_insert_rejects() {
    let text = r##"{
      format: 1
      name: control-toplevel-ref-insert
      suite: scenario
      spec: ["#refs", "§5.6", "#runtime", "§22.1"]
      package: {
        $liasse: 1
        $app: "t.snrb.ctlins@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: { $key: "id", id: "text", owner: { $ref: "/accounts" } }
          $mut: { add: ".tasks + { id: @id, owner: @owner }" }
          $public: { tasks: { $view: ".tasks { id }", $mut: { add: ".add" } } }
        }
        $data: { accounts: { a1: {} } }
      }
      steps: [
        { call: "public.tasks.add", args: { id: "t1", owner: "ghost" },
          expect: { outcome: rejected, violates: ["#refs", "§5.6"] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-toplevel-ref-insert");
}

// CONTROL 1b: a struct-nested ref to a VALID target commits and reads back — so
// the construct is fully supported; only its validity is unchecked.
#[test]
fn control_struct_nested_ref_valid_commits() {
    let text = r##"{
      format: 1
      name: control-struct-nested-ref-valid
      suite: scenario
      spec: ["#refs", "§5.6"]
      package: {
        $liasse: 1
        $app: "t.snrb.valid@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: { $key: "id", id: "text", meta: { owner: { $ref: "/accounts" } } }
          $mut: { add: ".tasks + { id: @id, meta: { owner: @owner } }" }
          $public: { tasks: { $view: ".tasks { id, meta }", $mut: { add: ".add" } } }
        }
        $data: { accounts: { a1: {} } }
      }
      steps: [
        { call: "public.tasks.add", args: { id: "t1", owner: "a1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.tasks", id: "w1", expect_init: { value: [ { id: "t1", meta: { owner: "a1" } } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-struct-nested-ref-valid");
}

// ── FINDING 2 ────────────────────────────────────────────────────────────────
// §21.1: `$on_delete: restrict` on a struct-nested ref MUST block deletion of the
// target while the referencing row survives. The delete instead SUCCEEDS.
#[test]
fn struct_nested_restrict_must_block_target_delete() {
    let text = r##"{
      format: 1
      name: struct-nested-restrict-blocks-delete
      suite: scenario
      spec: ["#deletion", "§21.1"]
      package: {
        $liasse: 1
        $app: "t.snrb.restrict@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: {
            $key: "id"
            id: "text"
            meta: { owner: { $ref: "/accounts", $on_delete: "restrict" } }
          }
          $mut: { del: ".accounts - @id" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { del: ".del" } }
            tasks: { $view: ".tasks { id }" }
          }
        }
        $data: { accounts: { a1: {} }, tasks: { t1: { meta: { owner: "a1" } } } }
      }
      steps: [
        { call: "public.accounts.del", args: { id: "a1" },
          expect: { outcome: rejected, violates: ["#deletion", "§21.1"] } }
        { watch: "public.accounts", id: "w1", expect_init: { value: [ { id: "a1" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "struct-nested-restrict-blocks-delete");
}

// CONTROL 2: the identical `restrict` ref at TOP level DOES block the delete.
#[test]
fn control_toplevel_restrict_blocks_target_delete() {
    let text = r##"{
      format: 1
      name: control-toplevel-restrict
      suite: scenario
      spec: ["#deletion", "§21.1"]
      package: {
        $liasse: 1
        $app: "t.snrb.ctlrestrict@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: { $key: "id", id: "text", owner: { $ref: "/accounts", $on_delete: "restrict" } }
          $mut: { del: ".accounts - @id" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { del: ".del" } }
            tasks: { $view: ".tasks { id }" }
          }
        }
        $data: { accounts: { a1: {} }, tasks: { t1: { owner: "a1" } } }
      }
      steps: [
        { call: "public.accounts.del", args: { id: "a1" },
          expect: { outcome: rejected, violates: ["#deletion", "§21.1"] } }
        { watch: "public.accounts", id: "w1", expect_init: { value: [ { id: "a1" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-toplevel-restrict");
}

// ── FINDING 3 ────────────────────────────────────────────────────────────────
// §21.1: `$on_delete: cascade` on a struct-nested ref MUST delete the containing
// row with the target. Instead the target is deleted and the task SURVIVES with a
// dangling struct-nested ref.
#[test]
fn struct_nested_cascade_must_delete_containing_row() {
    let text = r##"{
      format: 1
      name: struct-nested-cascade-deletes-row
      suite: scenario
      spec: ["#deletion", "§21.1", "#runtime", "§22.1"]
      package: {
        $liasse: 1
        $app: "t.snrb.cascade@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: {
            $key: "id"
            id: "text"
            meta: { owner: { $ref: "/accounts", $on_delete: "cascade" } }
          }
          $mut: { del: ".accounts - @id" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { del: ".del" } }
            tasks: { $view: ".tasks { id }" }
          }
        }
        $data: { accounts: { a1: {} }, tasks: { t1: { meta: { owner: "a1" } } } }
      }
      steps: [
        { call: "public.accounts.del", args: { id: "a1" }, expect: { outcome: ok, "...": true } }
        // §21.1 cascade: the task that referenced a1 through meta.owner is gone.
        { watch: "public.tasks", id: "w1", expect_init: { value: [] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "struct-nested-cascade-deletes-row");
}

// CONTROL 3: the identical `cascade` ref at TOP level DOES delete the row.
#[test]
fn control_toplevel_cascade_deletes_containing_row() {
    let text = r##"{
      format: 1
      name: control-toplevel-cascade
      suite: scenario
      spec: ["#deletion", "§21.1"]
      package: {
        $liasse: 1
        $app: "t.snrb.ctlcascade@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: { $key: "id", id: "text", owner: { $ref: "/accounts", $on_delete: "cascade" } }
          $mut: { del: ".accounts - @id" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { del: ".del" } }
            tasks: { $view: ".tasks { id }" }
          }
        }
        $data: { accounts: { a1: {} }, tasks: { t1: { owner: "a1" } } }
      }
      steps: [
        { call: "public.accounts.del", args: { id: "a1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.tasks", id: "w1", expect_init: { value: [] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-toplevel-cascade");
}

// ── FINDING 4 ────────────────────────────────────────────────────────────────
// §5.4: an atomic rekey MUST update every ref that targets the rekeyed row; the
// visible key becomes the new key while the relationship is preserved. A
// struct-nested inbound ref keeps the STALE prior key instead.
#[test]
fn struct_nested_inbound_ref_must_follow_rekey() {
    let text = r##"{
      format: 1
      name: struct-nested-ref-follows-rekey
      suite: scenario
      spec: ["#state-model", "§5.4", "#refs", "§5.6"]
      package: {
        $liasse: 1
        $app: "t.snrb.rekey@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: {
            $key: "id"
            id: "text"
            meta: { owner: { $ref: "/accounts" } }
          }
          $mut: { rekey: ".accounts[@old].id = @new" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { rekey: ".rekey" } }
            tasks: { $view: ".tasks { id, meta }" }
          }
        }
        $data: { accounts: { a1: {} }, tasks: { t1: { meta: { owner: "a1" } } } }
      }
      steps: [
        { call: "public.accounts.rekey", args: { old: "a1", new: "a2" }, expect: { outcome: ok, "...": true } }
        // §5.4: the ref follows the rekeyed incarnation -> visible key is now "a2".
        { watch: "public.tasks", id: "w1", expect_init: { value: [ { id: "t1", meta: { owner: "a2" } } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "struct-nested-ref-follows-rekey");
}

// CONTROL 4: the identical ref at TOP level DOES follow the rekey.
#[test]
fn control_toplevel_inbound_ref_follows_rekey() {
    let text = r##"{
      format: 1
      name: control-toplevel-rekey
      suite: scenario
      spec: ["#state-model", "§5.4", "#refs", "§5.6"]
      package: {
        $liasse: 1
        $app: "t.snrb.ctlrekey@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text" }
          tasks: { $key: "id", id: "text", owner: { $ref: "/accounts" } }
          $mut: { rekey: ".accounts[@old].id = @new" }
          $public: {
            accounts: { $view: ".accounts { id }", $mut: { rekey: ".rekey" } }
            tasks: { $view: ".tasks { id, owner }" }
          }
        }
        $data: { accounts: { a1: {} }, tasks: { t1: { owner: "a1" } } }
      }
      steps: [
        { call: "public.accounts.rekey", args: { old: "a1", new: "a2" }, expect: { outcome: ok, "...": true } }
        { watch: "public.tasks", id: "w1", expect_init: { value: [ { id: "t1", owner: "a2" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-toplevel-rekey");
}
