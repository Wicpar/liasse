//! RED-TEAM follow-on to Wave-17 Fix 2 (commit c77f410, `rekey_coerced`,
//! crates/liasse-runtime/src/migrate.rs:486). Fix 2 moved the enum-reorder
//! inbound-ref rewrite to run AFTER every moving row is re-placed, so a moving
//! referrer that sorts after its referent still follows on a pure reorder (§5.4).
//! The landed regression (`redteam_migration_enum_reorder_selfref`) exercised only
//! a SCALAR enum key with a SCALAR self-`$ref`. This file probes the GENERALIZATION
//! the fix claims ("every referrer — scalar `$ref` and `$set`-of-`$ref` alike —
//! now follows its target regardless of sort order"):
//!
//!   * a `$set`-of-`$ref` with THREE interleaved members where the holder sorts
//!     last (the fix's own comment claims this class is closed);
//!   * a COMPOSITE enum key `[status, id]` with a composite self-`$ref` where the
//!     referrer's source ordinal is larger than its referent's (the backward case);
//!   * a genuine label SWAP (A<->B bijection) with self-`$ref`s pointing BOTH ways.
//!
//! In every case the reorder is a pure §5.4 bijection over retained labels, so
//! §20.1 MUST ADMIT it and every inbound reference MUST resolve to its moved
//! target — the §20.1 final ref-integrity check rejects a dangling ref, so an
//! `outcome: ok` on the reorder load is exactly the assertion that all refs
//! followed. Every expectation is deducible from SPEC.md text alone (§5.4 refs
//! follow a rekey, §5.5 set-of-ref integrity, §5.9/§B.5 declaration-order ordinals,
//! §5.6/§22.1 ref integrity, §20.1 migration admission).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

fn run(name: &str, package_body: &str, steps: &str) -> CaseResult {
    let text = format!(
        r##"{{
  format: 1
  name: {name}
  suite: scenario
  spec: ["#evolution", "§20.1", "#state-model", "§5.4", "§5.5", "§5.9", "#deletion", "§5.6"]
  package: {{ $liasse: 1, $app: "t.reorder.fam@1.0.0", {package_body} }}
  steps: {steps}
}}"##
    );
    let case = Case::from_hjson(&text, Path::new(name), &BTreeSet::new()).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new(name), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult, context: &str) {
    for (index, step) in result.steps.iter().enumerate() {
        assert!(
            step.result.is_pass(),
            "{context}: step {index} ({}) did not pass: {:?}",
            step.action,
            step.result
        );
    }
}

// ---------------------------------------------------------------------------
// Case 1 — `$set`-of-`$ref`, THREE interleaved members, holder sorts LAST.
// ---------------------------------------------------------------------------

/// Enum-key `states` [a,b,c,d]; row `d` holds `deps = {ref a, ref b, ref c}`.
/// Every dep sorts before `d` at the source (a<b<c<d). The reorder reverses the
/// declaration order to [d,c,b,a], so `d` (source ord 3) is detached LAST and the
/// three targets a(0->3), b(1->2), c(2->1) all move. Fix 2 must rewrite all three
/// set members after every row is re-placed. §5.4/§5.5/§20.1: admitted, all refs
/// resolve.
const SETREF_PKG: &str = r##"$model: {
      states: {
        $key: "name"
        name: { $enum: ["a", "b", "c", "d"] }
        deps: { $set: { $ref: "/states" } }
      }
      $public: { states: { $view: ".states { name, deps, $sort: [name] }" } }
    }
    $data: { states: { "a": {}, "b": {}, "c": {}, "d": { deps: ["a", "b", "c"] } } }"##;

const SETREF_V2: &str = r##"$model: {
              states: {
                $key: "name"
                name: { $enum: ["d", "c", "b", "a"] }
                deps: { $set: { $ref: "/states" } }
              }
              $public: { states: { $view: ".states { name, deps, $sort: [name] }" } }
            }"##;

#[test]
fn setref_three_interleaved_members_follow_reorder() {
    let steps = format!(
        r##"[
          {{ watch: "public.states", id: "w0",
             expect_init: {{ value: [
               {{ name: "a", deps: {{ $unordered: [] }} }}
               {{ name: "b", deps: {{ $unordered: [] }} }}
               {{ name: "c", deps: {{ $unordered: [] }} }}
               {{ name: "d", deps: {{ $unordered: ["a", "b", "c"] }} }}
             ] }} }}
          {{ host_load: {{ package: {{ $liasse: 1, $app: "t.reorder.fam@2.0.0", {SETREF_V2} }} }},
             expect: {{ outcome: ok }} }}
          {{ expect_view: {{ watch: "w0", value: [
               {{ name: "d", deps: {{ $unordered: ["a", "b", "c"] }} }}
               {{ name: "c", deps: {{ $unordered: [] }} }}
               {{ name: "b", deps: {{ $unordered: [] }} }}
               {{ name: "a", deps: {{ $unordered: [] }} }}
             ] }} }}
        ]"##
    );
    let result = run("setref-three-interleaved", SETREF_PKG, &steps);
    assert_all_pass(&result, "set-of-ref three interleaved members follow a reorder");
}

// ---------------------------------------------------------------------------
// Case 2 — COMPOSITE enum key [status, id], backward composite self-ref.
// ---------------------------------------------------------------------------

/// Composite key `[status, id]`, `status` an enum draft(0),active(1),closed(2).
/// `active:t2` supersedes `draft:t1` and `closed:t3` supersedes `active:t2` — each
/// referrer's source ordinal is LARGER than its referent's (the backward case the
/// fix targets). The reorder [active,closed,draft] moves every row, so each
/// referrer is detached while its referent is re-placed; Fix 2 must rewrite the
/// composite ref after all rows are placed. §5.4/§20.1: admitted, refs resolve.
const COMP_PKG: &str = r##"$model: {
      things: {
        $key: ["status", "id"]
        status: { $enum: ["draft", "active", "closed"] }
        id: "text"
        supersedes: { $ref: "/things", $optional: true }
      }
      $public: { things: { $view: ".things { status, id, supersedes, $sort: [status, id] }" } }
    }
    $data: { things: {
      "draft:t1": {}
      "active:t2": { supersedes: { status: "draft", id: "t1" } }
      "closed:t3": { supersedes: { status: "active", id: "t2" } }
    } }"##;

const COMP_V2: &str = r##"$model: {
              things: {
                $key: ["status", "id"]
                status: { $enum: ["active", "closed", "draft"] }
                id: "text"
                supersedes: { $ref: "/things", $optional: true }
              }
              $public: { things: { $view: ".things { status, id, supersedes, $sort: [status, id] }" } }
            }"##;

#[test]
fn composite_key_backward_ref_follows_reorder() {
    let steps = format!(
        r##"[
          {{ watch: "public.things", id: "w0",
             expect_init: {{ value: [
               {{ status: "draft",  id: "t1", supersedes: "$absent" }}
               {{ status: "active", id: "t2", supersedes: ["draft", "t1"] }}
               {{ status: "closed", id: "t3", supersedes: ["active", "t2"] }}
             ] }} }}
          {{ host_load: {{ package: {{ $liasse: 1, $app: "t.reorder.fam@2.0.0", {COMP_V2} }} }},
             expect: {{ outcome: ok }} }}
          {{ expect_view: {{ watch: "w0", value: [
               {{ status: "active", id: "t2", supersedes: ["draft", "t1"] }}
               {{ status: "closed", id: "t3", supersedes: ["active", "t2"] }}
               {{ status: "draft",  id: "t1", supersedes: "$absent" }}
             ] }} }}
        ]"##
    );
    let result = run("composite-key-backward-ref", COMP_PKG, &steps);
    assert_all_pass(&result, "composite-key backward composite ref follows a reorder");
}

// ---------------------------------------------------------------------------
// Case 3 — genuine label SWAP A<->B with self-refs BOTH directions.
// ---------------------------------------------------------------------------

/// Enum [a,b] swapped to [b,a]; `a` supersedes `b` and `b` supersedes `a`. Both
/// rows move (a 0->1, b 1->0) and reference each other, so the detach-then-replace
/// must not read a false DuplicateKey and both refs must be rewritten. §5.4/§20.1.
const SWAP_PKG: &str = r##"$model: {
      states: {
        $key: "name"
        name: { $enum: ["a", "b"] }
        supersedes: { $ref: "/states", $optional: true }
      }
      $public: { states: { $view: ".states { name, supersedes, $sort: [name] }" } }
    }
    $data: { states: {
      "a": { supersedes: "b" }
      "b": { supersedes: "a" }
    } }"##;

const SWAP_V2: &str = r##"$model: {
              states: {
                $key: "name"
                name: { $enum: ["b", "a"] }
                supersedes: { $ref: "/states", $optional: true }
              }
              $public: { states: { $view: ".states { name, supersedes, $sort: [name] }" } }
            }"##;

#[test]
fn label_swap_with_bidirectional_refs_is_admitted() {
    let steps = format!(
        r##"[
          {{ watch: "public.states", id: "w0",
             expect_init: {{ value: [
               {{ name: "a", supersedes: "b" }}
               {{ name: "b", supersedes: "a" }}
             ] }} }}
          {{ host_load: {{ package: {{ $liasse: 1, $app: "t.reorder.fam@2.0.0", {SWAP_V2} }} }},
             expect: {{ outcome: ok }} }}
          {{ expect_view: {{ watch: "w0", value: [
               {{ name: "b", supersedes: "a" }}
               {{ name: "a", supersedes: "b" }}
             ] }} }}
        ]"##
    );
    let result = run("label-swap-bidirectional-refs", SWAP_PKG, &steps);
    assert_all_pass(&result, "A<->B swap with bidirectional refs is admitted and refs follow");
}

// ---------------------------------------------------------------------------
// Case 4 — CROSS-COLLECTION ref into the reordered collection.
// ---------------------------------------------------------------------------

/// A separate `pointers` collection references the reordered `states`. `p1 -> c`
/// where `c` sorts last in `states` and moves to the front on the reorder. The
/// referrer is in another collection (never moves), so `rewrite_inbound_refs_across`
/// must still rewrite it when scanning the whole prospective. §5.4/§20.1.
const XCOLL_PKG: &str = r##"$model: {
      states: { $key: "name", name: { $enum: ["a", "b", "c"] } }
      pointers: { $key: "id", id: "text", to: { $ref: "/states" } }
      $public: {
        states: { $view: ".states { name, $sort: [name] }" }
        pointers: { $view: ".pointers { id, to, $sort: [id] }" }
      }
    }
    $data: {
      states: { "a": {}, "b": {}, "c": {} }
      pointers: { "p1": { to: "c" } }
    }"##;

const XCOLL_V2: &str = r##"$model: {
              states: { $key: "name", name: { $enum: ["c", "b", "a"] } }
              pointers: { $key: "id", id: "text", to: { $ref: "/states" } }
              $public: {
                states: { $view: ".states { name, $sort: [name] }" }
                pointers: { $view: ".pointers { id, to, $sort: [id] }" }
              }
            }"##;

#[test]
fn cross_collection_ref_follows_reorder() {
    let steps = format!(
        r##"[
          {{ watch: "public.pointers", id: "wp",
             expect_init: {{ value: [ {{ id: "p1", to: "c" }} ] }} }}
          {{ host_load: {{ package: {{ $liasse: 1, $app: "t.reorder.fam@2.0.0", {XCOLL_V2} }} }},
             expect: {{ outcome: ok }} }}
          {{ expect_view: {{ watch: "wp", value: [ {{ id: "p1", to: "c" }} ] }} }}
        ]"##
    );
    let result = run("cross-collection-ref", XCOLL_PKG, &steps);
    assert_all_pass(&result, "cross-collection ref follows a reorder");
}
