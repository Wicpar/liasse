//! RED-TEAM probe: an enum-KEY reorder migration silently drops inbound
//! references between MOVING rows, wrongly rejecting a valid §5.4 reorder.
//!
//! The a0e1f36 fix (`rekey_coerced`, crates/liasse-runtime/src/migrate.rs:486)
//! re-addresses every row whose coerced enum key lands at a new declaration-order
//! ordinal (§5.9/B.5) and "reuses the ordinary rekey's inbound-reference rewrite
//! ... so a reference that keyed on a moved row follows it to the new key (§5.4)".
//! It does this by DETACHING every moving row from the prospective state first
//! (migrate.rs:511-516), then re-placing each and calling
//! `rewrite_inbound_refs_across` (interp.rs:1851) — which only scans rows CURRENTLY
//! placed in the prospective state (interp.rs:1863).
//!
//! A moving referrer whose source-ordinal key sorts AFTER its referent's is still
//! detached when the referent is re-placed, so its outbound ref is never
//! rewritten and keeps the SOURCE ordinal. Because `EnumValue` equality compares
//! ordinal AND label (crates/liasse-value/src/enumeration.rs:93-100) and ref
//! admission matches a ref against its target by value equality
//! (`refid::ref_identity`, §5.6/A.9), the stale-ordinal ref no longer resolves to
//! its (moved) target — a dangling reference. The §20.1 final check rejects the
//! whole migration, so a valid pure reorder is refused (E.9 "the current package
//! stays active"), contradicting §5.4 ("refs follow a rekey") and the fix's own
//! stated contract.
//!
//! The three tests share ONE reorder ([draft,active,closed] -> [active,closed,
//! draft]); only the self-ref chain's DIRECTION changes. This isolates the defect
//! to the rewrite's processing order, not the reorder or the coercion:
//!
//! - no refs at all -> migration admitted (reorder itself is fine);
//! - refs where each referrer sorts BEFORE its referent (source ordinal smaller)
//!   -> admitted (the referrer is already re-placed when the referent rewrites);
//! - refs where each referrer sorts AFTER its referent -> REJECTED (the bug).
//!
//! Every expectation is deducible from SPEC.md text alone (§5.4 refs-follow-rekey,
//! §5.9/B.5 declaration-order ordinals, §5.6 ref integrity, §20.1 migration
//! admission); none encodes implementation behaviour.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

/// A single `states` collection keyed by an enum `name`, with an optional self
/// `$ref`. `DATA` seeds the ref chain; `STEPS` drives the reorder migration.
const APP: &str = r##"{
  format: 1
  name: enum-reorder-selfref
  suite: scenario
  spec: ["#evolution", "§20.1", "#state-model", "§5.4", "§5.9", "#deletion", "§5.6"]
  package: {
    $liasse: 1
    $app: "t.mig.reorder@1.0.0"
    $model: {
      states: {
        $key: "name"
        name: { $enum: ["draft", "active", "closed"] }
        supersedes: { $ref: "/states", $optional: true }
      }
      $public: {
        states: { $view: ".states { name, supersedes, $sort: [name] }" }
      }
    }
    $data: { states: DATA }
  }
  steps: STEPS
}"##;

/// The reorder target: same labels, new declaration order. Every row's ordinal
/// changes (draft 0->2, active 1->0, closed 2->1), so every row is re-addressed.
const V2_MODEL: &str = r##"$model: {
              states: {
                $key: "name"
                name: { $enum: ["active", "closed", "draft"] }
                supersedes: { $ref: "/states", $optional: true }
              }
              $public: {
                states: { $view: ".states { name, supersedes, $sort: [name] }" }
              }
            }"##;

fn run_app(name: &str, data: &str, steps: &str) -> CaseResult {
    let text = APP.replace("DATA", data).replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new(name), &BTreeSet::new()).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new(name), SuiteKind::Red, &case)
}

/// Build the host_load reorder step plus a post-migration view assertion.
fn reorder_steps(init_view: &str, post_view: &str) -> String {
    format!(
        r##"[
          {{ watch: "public.states", id: "w0", expect_init: {{ value: {init_view} }} }}
          {{ host_load: {{ package: {{ $liasse: 1, $app: "t.mig.reorder@2.0.0", {V2_MODEL} }} }},
             expect: {{ outcome: ok }} }}
          {{ expect_view: {{ watch: "w0", value: {post_view} }} }}
        ]"##
    )
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

/// CONTROL — the SAME reorder with NO inbound references. Every row moves, none
/// references another, so `rekey_coerced` re-addresses them with nothing to
/// rewrite. §20.1 admits the reorder. Must PASS.
#[test]
fn reorder_without_refs_is_admitted() {
    let init = r##"[
        { name: "draft",  supersedes: "$absent" }
        { name: "active", supersedes: "$absent" }
        { name: "closed", supersedes: "$absent" }
    ]"##;
    let post = r##"[
        { name: "active", supersedes: "$absent" }
        { name: "closed", supersedes: "$absent" }
        { name: "draft",  supersedes: "$absent" }
    ]"##;
    let data = r##"{ "draft": { }, "active": { }, "closed": { } }"##;
    let result = run_app("enum-reorder-norefs", data, &reorder_steps(init, post));
    assert_all_pass(&result, "reorder without refs");
}

/// CONTROL — refs where each referrer's SOURCE ordinal is SMALLER than its
/// referent's (draft[0]->active[1], active[1]->closed[2]). The referrer is
/// re-placed before the referent, so the referent's rewrite pass sees it and
/// rewrites the ref (§5.4). Must PASS — proving refs DO follow a reorder when the
/// rewrite order happens to cover them.
#[test]
fn reorder_refs_referrer_before_referent_is_admitted() {
    let init = r##"[
        { name: "draft",  supersedes: "active" }
        { name: "active", supersedes: "closed" }
        { name: "closed", supersedes: "$absent" }
    ]"##;
    let post = r##"[
        { name: "active", supersedes: "closed" }
        { name: "closed", supersedes: "$absent" }
        { name: "draft",  supersedes: "active" }
    ]"##;
    let data = r##"{ "draft": { supersedes: "active" }, "active": { supersedes: "closed" }, "closed": { } }"##;
    let result = run_app("enum-reorder-refs-fwd", data, &reorder_steps(init, post));
    assert_all_pass(&result, "reorder with referrer-before-referent refs");
}

/// BUG — refs where each referrer's SOURCE ordinal is LARGER than its referent's
/// (active[1]->draft[0], closed[2]->active[1]). The referrer is still detached
/// when the referent is re-placed, so its outbound ref is never rewritten, keeps
/// the source ordinal, and (EnumValue eq includes the ordinal) dangles — the
/// §20.1 final ref check rejects the whole valid reorder. CURRENTLY FAILS at the
/// host_load step (observed `rejected`, expected `ok`).
#[test]
fn reorder_refs_referrer_after_referent_is_admitted() {
    let init = r##"[
        { name: "draft",  supersedes: "$absent" }
        { name: "active", supersedes: "draft" }
        { name: "closed", supersedes: "active" }
    ]"##;
    let post = r##"[
        { name: "active", supersedes: "draft" }
        { name: "closed", supersedes: "active" }
        { name: "draft",  supersedes: "$absent" }
    ]"##;
    let data = r##"{ "draft": { }, "active": { supersedes: "draft" }, "closed": { supersedes: "active" } }"##;
    let result = run_app("enum-reorder-refs-bwd", data, &reorder_steps(init, post));
    assert_all_pass(&result, "reorder with referrer-after-referent refs");
}
