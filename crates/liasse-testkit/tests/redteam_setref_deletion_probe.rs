//! RED-TEAM cross-cutting probe: `$set`-of-`$ref` membership threaded through
//! deletion planning, erasure, rekeying, incarnation identity, and live views.
//!
//! Every single-subsystem surface here has been probed dry; this battery attacks
//! the SEAMS between them in one flow:
//!
//! - §21.1 × §5.5/§5.6: a set-member `cascade` is a **DropMember** (the member
//!   goes, the containing row survives) while a scalar `none` on the SAME
//!   surviving row is a field patch — one deletion commit must apply both.
//! - §8.7 × §5.6: delete + reinsert of the same key inside ONE mutation program
//!   creates a NEW incarnation mid-transition; neither the cleared scalar ref nor
//!   the dropped set membership may re-attach to it.
//! - §5.4 × §5.6 × §5.5/§B.1: an atomic rekey keeps refs attached to the
//!   incarnation, so a set of refs must READ the new key and RE-SORT by it
//!   (`ref<T>` orders by target key, B.1) — and a rekey is not a deletion, so no
//!   `$on_delete` may fire.
//! - §21.2 step 1 × §21.1: `erase(row)` plans "the same live removal and
//!   `$on_delete` effects as ordinary deletion" — so erasure of a shared target
//!   must clear the scalar ref AND drop the set member, coherently visible on a
//!   live watcher (§12.2).
//! - §21.1 restrict on a set MEMBER: the target is preserved while the membership
//!   exists, and becomes deletable the moment an ordinary set-mutation removes
//!   that membership.
//!
//! State is seeded through `$data` (§9.1) — the corpus-proven path for
//! set-of-ref membership (`06/set-ref-composite-projects-in-key-order`) — so the
//! probes isolate the deletion/rekey seams rather than the §8.3
//! insert-parameter-inference path.
//!
//! Every expectation is deducible from SPEC.md text alone (anchors cited per
//! test); none encodes implementation behavior.
//!
//! # CONFIRMED BUG (the four failing tests below reproduce one root cause)
//!
//! §5.5 set mutations (`set_field + v` / `set_field - v`) over a `$set` of
//! `$ref` operate on the WRONG value representation.
//! `Interp::set_mutate` (crates/liasse-runtime/src/interp.rs:1110-1136) unions/
//! differences the raw evaluated operand `Value` into the stored member set
//! without decoding it to the set's ELEMENT type. Stored set-of-ref members are
//! `Value::Ref` (what `$data` seeding stores, and what the §21.1 planner walks —
//! crates/liasse-runtime/src/cascade.rs:86-111 via `ref_key`,
//! cascade.rs:226-235), while the operand key text evaluates to `Value::Text`;
//! `Value`'s total order discriminates variants
//! (crates/liasse-value/src/value.rs:138), so `Text("a1") != Ref(->"a1")`.
//! Four spec-cited consequences, each reproduced below:
//!
//! 1. Removing a PRESENT membership by its §A.9 value silently no-ops
//!    (`unchanged`, member survives) — `restrict_set_member_unblocks_with_
//!    literal_removal`.
//! 2. Therefore a `restrict`-protected target can NEVER be unblocked through any
//!    application-expressible flow — `restrict_set_member_blocks_until_
//!    membership_removed`.
//! 3. Adding an EXISTING membership commits a duplicate representation; the live
//!    view renders the same application-visible member twice (`["a1","a1"]`) —
//!    `adding_existing_ref_member_by_key_value_collapses`.
//! 4. A mutation-added membership is carried as `Value::Text`, which the §21.1
//!    planner never walks: deleting the `restrict`-protected target is ADMITTED
//!    and a dangling application-visible reference is committed (§22.1 state-
//!    constraint breach) — `mutation_added_membership_still_restricts_target_
//!    deletion`.
//!
//! The add path DOES validate the operand resolves to an existing row
//! (`adding_nonexistent_ref_member_rejects` passes), then stores the
//! unconverted text — so the decode seam sits after validation, before staging.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

/// Accounts referenced two ways from `docs`: a scalar optional `owner` ref with
/// `$on_delete: none`, and a `reviewers` set of refs with `$on_delete: cascade`
/// (per §5.6/§21.1 the set-member cascade drops the MEMBER, not the doc row).
/// `del_reinsert` performs delete + same-key reinsert in ONE program (§8.7).
/// Seeded: accounts a1 "A", a2 "B"; doc d1 owned by a1, reviewers {a1, a2}.
const APP: &str = r##"{
  format: 1
  name: setref-deletion-probe
  suite: scenario
  spec: ["#deletion", "§21.1", "#state-model", "§5.5", "§5.6", "#clients", "§12.2"]
  package: {
    $liasse: 1
    $app: "t.srp@1.0.0"
    $model: {
      accounts: {
        $key: "id"
        id: "text"
        name: "text"
      }
      docs: {
        $key: "id"
        id: "text"
        owner: { $ref: "/accounts", $optional: true, $on_delete: "none" }
        reviewers: { $set: { $ref: "/accounts", $on_delete: "cascade" } }
      }
      $public: {
        docs: {
          $view: ".docs { id, owner, reviewers }"
          $mut: {
            del_account: ".accounts - @id"
            del_reinsert: [
              ".accounts - @id"
              ".accounts + { id: @id, name: @name }"
            ]
            rekey_account: ".accounts[@old].id = @new"
          }
        }
        accounts: {
          $view: ".accounts { id, $sort: [id] }"
        }
      }
    }
    $data: {
      accounts: {
        a1: { name: "A" }
        a2: { name: "B" }
      }
      docs: {
        d1: { owner: "a1", reviewers: ["a1", "a2"] }
      }
    }
  }
  steps: STEPS
}"##;

/// The restrict variant: the set member's policy is `restrict`, so the target is
/// preserved while the membership exists (§21.1); `unreview` removes the
/// membership through an ordinary set mutation (§5.5), unblocking deletion.
/// Seeded: account a1; doc d1 with reviewers {a1}.
const APP_RESTRICT: &str = r##"{
  format: 1
  name: setref-restrict-probe
  suite: scenario
  spec: ["#deletion", "§21.1", "#state-model", "§5.5", "§5.6"]
  package: {
    $liasse: 1
    $app: "t.srr@1.0.0"
    $model: {
      accounts: {
        $key: "id"
        id: "text"
        name: "text"
      }
      docs: {
        $key: "id"
        id: "text"
        reviewers: { $set: { $ref: "/accounts", $on_delete: "restrict" } }
      }
      $public: {
        docs: {
          $view: ".docs { id, reviewers }"
          $mut: {
            del_account: ".accounts - @id"
            unreview: ".docs[@doc].reviewers - @account"
            unreview_lit: ".docs['d1'].reviewers - 'a1'"
            review_dup: ".docs['d1'].reviewers + 'a1'"
            review_ghost: ".docs['d1'].reviewers + 'zz'"
          }
        }
      }
    }
    $data: {
      accounts: {
        a1: { name: "A" }
      }
      docs: {
        d1: { reviewers: ["a1"] }
      }
    }
  }
  steps: STEPS
}"##;

/// The dangling-reference escalation app: the ONLY membership of a1 is the one a
/// mutation adds (the doc is seeded with an EMPTY reviewers set), under a
/// `restrict` policy. §21.1: once that membership exists, deleting a1 MUST be
/// rejected while the membership survives.
const APP_DANGLE: &str = r##"{
  format: 1
  name: setref-dangle-probe
  suite: scenario
  spec: ["#deletion", "§21.1", "#state-model", "§5.5", "§5.6"]
  package: {
    $liasse: 1
    $app: "t.srd@1.0.0"
    $model: {
      accounts: {
        $key: "id"
        id: "text"
        name: "text"
      }
      docs: {
        $key: "id"
        id: "text"
        reviewers: { $set: { $ref: "/accounts", $on_delete: "restrict" } }
      }
      $public: {
        docs: {
          $view: ".docs { id, reviewers }"
          $mut: {
            del_account: ".accounts - @id"
            review: ".docs['d1'].reviewers + 'a1'"
          }
        }
      }
    }
    $data: {
      accounts: {
        a1: { name: "A" }
      }
      docs: {
        d1: { reviewers: [] }
      }
    }
  }
  steps: STEPS
}"##;

/// The erasure variant: `erase(.accounts[@id])` is the explicitly exposed
/// erasure call (§21.2); its step 1 plans the SAME live removal and `$on_delete`
/// effects as ordinary deletion, so the scalar `none` clear and the set-member
/// drop must both land in the erasure commit.
/// Seeded: accounts a1, a2; doc d1 owned by a1, reviewers {a1, a2}.
const APP_ERASE: &str = r##"{
  format: 1
  name: setref-erase-probe
  suite: scenario
  spec: ["#deletion", "§21.2", "§21.1", "§5.5", "§5.6", "§12.2"]
  package: {
    $liasse: 1
    $app: "t.sre@1.0.0"
    $model: {
      accounts: {
        $key: "id"
        id: "text"
        name: "text"
      }
      docs: {
        $key: "id"
        id: "text"
        owner: { $ref: "/accounts", $optional: true, $on_delete: "none" }
        reviewers: { $set: { $ref: "/accounts", $on_delete: "cascade" } }
      }
      $mut: {
        erase_account: "return erase(.accounts[@id])"
      }
      $public: {
        docs: {
          $view: ".docs { id, owner, reviewers }"
          $mut: {
            erase: ".erase_account"
          }
        }
        accounts: {
          $view: ".accounts { id, $sort: [id] }"
        }
      }
    }
    $data: {
      accounts: {
        a1: { name: "A" }
        a2: { name: "B" }
      }
      docs: {
        d1: { owner: "a1", reviewers: ["a1", "a2"] }
      }
    }
  }
  steps: STEPS
}"##;

fn run_app(app: &str, name: &str, steps: &str) -> CaseResult {
    let text = app.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new(name), &BTreeSet::new()).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new(name), SuiteKind::Red, &case)
}

fn assert_steps_pass(result: &CaseResult, upto: usize, context: &str) {
    for index in 0..upto {
        let step = result
            .steps
            .get(index)
            .unwrap_or_else(|| panic!("{context}: no step {index}: {:?}", result.steps));
        assert!(
            step.result.is_pass(),
            "{context}: step {index} ({}) did not pass: {:?}",
            step.action,
            step.result
        );
    }
}

/// The baseline watch every main-app probe opens first: pins the seeded state
/// (§9.1) and gives §12.2 a live subscription to hold coherent afterwards.
const BASELINE_WATCH: &str = r##"
  { watch: "public.docs", id: "w1",
    expect_init: { value: [ { id: "d1", owner: "a1", reviewers: ["a1", "a2"] } ] } }
"##;

/// §21.1/§5.6: deleting a target that is a set MEMBER drops the member, not the
/// containing row; the sibling member and the (untargeted) scalar ref survive.
/// The live watcher must show the post-commit view (§12.2).
#[test]
fn cascade_on_set_member_drops_member_not_containing_row() {
    let steps = format!(
        r##"[
          {BASELINE_WATCH}
          {{ call: "public.docs.del_account", args: {{ id: "a2" }}, expect: {{ outcome: ok }} }}
          {{ expect_view: {{ watch: "w1", value: [ {{ id: "d1", owner: "a1", reviewers: ["a1"] }} ] }} }}
        ]"##
    );
    let result = run_app(APP, "setref-deletion-probe", &steps);
    assert_steps_pass(&result, 3, "cascade drops member only");
}

/// §21.1 atomic plan: ONE deletion commit fires BOTH policies on the SAME
/// surviving row — the scalar `none` clears `owner` (a field patch) and the set
/// cascade drops the `a1` membership (a member removal). Disjoint coordinates
/// combine (§21.1); the doc row survives with both effects applied.
#[test]
fn one_commit_clears_scalar_ref_and_drops_set_member() {
    let steps = format!(
        r##"[
          {BASELINE_WATCH}
          {{ call: "public.docs.del_account", args: {{ id: "a1" }}, expect: {{ outcome: ok }} }}
          {{ expect_view: {{ watch: "w1", value: [ {{ id: "d1", owner: "$absent", reviewers: ["a2"] }} ] }} }}
        ]"##
    );
    let result = run_app(APP, "setref-deletion-probe", &steps);
    assert_steps_pass(&result, 3, "scalar clear + member drop in one commit");
}

/// §8.7 × §5.6: `del_reinsert` deletes a1 and reinserts the same key in ONE
/// program. Statement order applies the full §21.1 plan first (owner cleared,
/// membership dropped); the reinsert then creates a NEW incarnation which must
/// NOT recapture the cleared ref or the dropped membership ("Deleting and
/// reinserting the same key creates a new incarnation and does not transfer
/// existing refs", §5.6). The account itself exists again afterwards.
#[test]
fn delete_and_reinsert_same_key_in_one_program_is_new_incarnation() {
    let steps = format!(
        r##"[
          {BASELINE_WATCH}
          {{ watch: "public.accounts", id: "wa",
             expect_init: {{ value: [ {{ id: "a1" }}, {{ id: "a2" }} ] }} }}
          {{ call: "public.docs.del_reinsert", args: {{ id: "a1", name: "A2" }},
             expect: {{ outcome: ok }} }}
          {{ expect_view: {{ watch: "w1", value: [ {{ id: "d1", owner: "$absent", reviewers: ["a2"] }} ] }} }}
          {{ expect_view: {{ watch: "wa", value: [ {{ id: "a1" }}, {{ id: "a2" }} ] }} }}
        ]"##
    );
    let result = run_app(APP, "setref-deletion-probe", &steps);
    assert_steps_pass(&result, 5, "delete+reinsert one program");
}

/// §5.4 × §5.6 × §5.5/§B.1 × §12.2: an atomic rekey keeps every ref attached to
/// the incarnation, so the scalar ref READS the new key and the set of refs both
/// reads it and RE-SORTS by it (`ref<T>` orders by target key order, B.1: text
/// "z9" sorts after "a2"). A rekey is not a deletion (§5.4), so neither
/// `$on_delete` fires: both members remain.
#[test]
fn rekey_of_set_member_reads_and_reorders_by_new_key() {
    let steps = format!(
        r##"[
          {BASELINE_WATCH}
          {{ call: "public.docs.rekey_account", args: {{ old: "a1", new: "z9" }},
             expect: {{ outcome: ok }} }}
          {{ expect_view: {{ watch: "w1", value: [ {{ id: "d1", owner: "z9", reviewers: ["a2", "z9"] }} ] }} }}
        ]"##
    );
    let result = run_app(APP, "setref-deletion-probe", &steps);
    assert_steps_pass(&result, 3, "rekey rereads and reorders set membership");
}

/// §21.1: a `restrict` on a set MEMBER preserves the target while the membership
/// exists; removing the membership through an ordinary §5.5 set mutation makes
/// the same deletion admissible. (The restrict edge must be per-MEMBER: after
/// `unreview` no membership of a1 remains anywhere, so nothing blocks.)
#[test]
fn restrict_set_member_blocks_until_membership_removed() {
    let steps = r##"[
      { call: "public.docs.del_account", args: { id: "a1" },
        expect: { outcome: rejected, violates: ["§21.1"] } }
      { call: "public.docs.unreview", args: { doc: "d1", account: "a1" }, expect: { outcome: ok } }
      // GUARD: the membership really is gone before the retry (§5.5) — this
      // discriminates a silent no-op removal from a stale restrict planner.
      { watch: "public.docs", id: "w0",
        expect_init: { value: [ { id: "d1", reviewers: [] } ] } }
      { call: "public.docs.del_account", args: { id: "a1" }, expect: { outcome: ok } }
      { watch: "public.docs", id: "w1",
        expect_init: { value: [ { id: "d1", reviewers: [] } ] } }
    ]"##;
    let result = run_app(APP_RESTRICT, "setref-restrict-probe", steps);
    assert_steps_pass(&result, 5, "restrict member blocks then unblocks");
}

/// The literal-form variant of the same §21.1 unblock: the membership removal
/// uses fixed literals (`.docs['d1'].reviewers - 'a1'`), removing the §8.3
/// parameter-inference path from the flow entirely. If THIS passes while the
/// parameterized form fails, the defect is the parameter decode of a set-member
/// value, not the restrict planner. §5.5/§A.9: the member IS present — a ref's
/// application-visible value is its target's current typed key, so `'a1'` names
/// the member — and its removal is a state change, so the call completes
/// `committed` (§8.9) and the membership is gone.
#[test]
fn restrict_set_member_unblocks_with_literal_removal() {
    let steps = r##"[
      { call: "public.docs.unreview_lit", args: {},
        expect: { outcome: ok, completion: committed } }
      { watch: "public.docs", id: "w0",
        expect_init: { value: [ { id: "d1", reviewers: [] } ] } }
      { call: "public.docs.del_account", args: { id: "a1" }, expect: { outcome: ok } }
    ]"##;
    let result = run_app(APP_RESTRICT, "setref-restrict-probe", steps);
    assert_steps_pass(&result, 3, "restrict member unblocks via literal removal");
}

/// §5.5 dedup through the ref value duality: adding `'a1'` to a set of refs that
/// already CONTAINS the a1 membership must leave the set unchanged ("adding an
/// existing member leaves the set unchanged" — the authoring/wire value of a ref
/// member IS the target key, §A.9). The set must never present two members with
/// the same application-visible identity.
#[test]
fn adding_existing_ref_member_by_key_value_collapses() {
    let steps = r##"[
      { call: "public.docs.review_dup", args: {}, expect: { outcome: ok, completion: unchanged } }
      { watch: "public.docs", id: "w0",
        expect_init: { value: [ { id: "d1", reviewers: ["a1"] } ] } }
    ]"##;
    let result = run_app(APP_RESTRICT, "setref-restrict-probe", steps);
    assert_steps_pass(&result, 2, "add of existing ref member collapses");
}

/// §5.6 referential integrity through the same seam: a ref "MUST resolve to an
/// existing row", so adding membership of a NONEXISTENT account must reject the
/// transition. If the set mutation admits a raw text member without ref
/// admission, the §21.1 planner (which walks only ref-valued members) never sees
/// it either — a dangling pseudo-member invisible to deletion planning.
#[test]
fn adding_nonexistent_ref_member_rejects() {
    let steps = r##"[
      { call: "public.docs.review_ghost", args: {},
        expect: { outcome: rejected, violates: ["§5.6"] } }
      { watch: "public.docs", id: "w0",
        expect_init: { value: [ { id: "d1", reviewers: ["a1"] } ] } }
    ]"##;
    let result = run_app(APP_RESTRICT, "setref-restrict-probe", steps);
    assert_steps_pass(&result, 2, "add of nonexistent ref member rejects");
}

/// §21.1 × §5.5 through the mutation-added membership: the ONLY membership of
/// a1 is added by an ordinary §5.5 set mutation (the seed set is empty), under
/// `restrict`. §21.1: "restrict — reject deletion while the ref exists" — the
/// membership exists (the watch proves it renders), so `del_account` MUST be
/// rejected and the membership must survive. If the mutation-admitted member is
/// carried in a representation the §21.1 planner does not walk, the deletion is
/// admitted and a DANGLING application-visible reference is committed — the
/// §22.1 state-constraint breach ("reference validity ... hold[s] in every
/// committed state").
#[test]
fn mutation_added_membership_still_restricts_target_deletion() {
    let steps = r##"[
      { call: "public.docs.review", args: {}, expect: { outcome: ok } }
      { watch: "public.docs", id: "w0",
        expect_init: { value: [ { id: "d1", reviewers: ["a1"] } ] } }
      { call: "public.docs.del_account", args: { id: "a1" },
        expect: { outcome: rejected, violates: ["§21.1"] } }
      { expect_view: { watch: "w0", value: [ { id: "d1", reviewers: ["a1"] } ] } }
    ]"##;
    let result = run_app(APP_DANGLE, "setref-dangle-probe", steps);
    assert_steps_pass(&result, 4, "mutation-added membership restricts deletion");
}

/// §21.2 step 1 × §21.1 × §12.2: `erase(row)` "plans the same live removal and
/// `$on_delete` effects as ordinary deletion" — so erasing a1 must clear the
/// scalar `owner` AND drop the a1 membership in the SAME erasure commit, and the
/// live watcher must see the coherent post-erasure view. The erased account is
/// gone from live state; the sibling account survives.
#[test]
fn erase_of_shared_target_applies_full_deletion_plan() {
    let steps = r##"[
      { watch: "public.docs", id: "w1",
        expect_init: { value: [ { id: "d1", owner: "a1", reviewers: ["a1", "a2"] } ] } }
      { watch: "public.accounts", id: "wa",
        expect_init: { value: [ { id: "a1" }, { id: "a2" } ] } }
      { call: "public.docs.erase", args: { id: "a1" }, expect: { outcome: ok } }
      { expect_view: { watch: "w1", value: [ { id: "d1", owner: "$absent", reviewers: ["a2"] } ] } }
      { expect_view: { watch: "wa", value: [ { id: "a2" } ] } }
    ]"##;
    let result = run_app(APP_ERASE, "setref-erase-probe", steps);
    assert_steps_pass(&result, 5, "erase applies full deletion plan");
}
