//! §8.3 × §5.5/§5.6/§21.1: a row INSERT whose `$set` of `$ref` field is
//! initialized from a call parameter (`reviewers: @reviewers`).
//!
//! The companion `redteam_setref_deletion_probe` deliberately seeds membership
//! through `$data` to isolate the deletion/rekey seams from the §8.3
//! insert-parameter-inference path. This guard exercises that inference path
//! directly: the parameter's type is inferred from the target field, so a set of
//! refs supplied on the wire (`["a1","a2"]`) must decode to the target
//! collection's key type (A.9) — a `ref` whose visible value IS its target's key
//! — not a placeholder. If the inferred element type strands the ref key at the
//! unresolved `json` placeholder, each decoded member's key never equals its
//! target's real key identity, so the insert either rejects every member as
//! dangling (§5.6) or, worse, commits members the §21.1 planner cannot walk.
//!
//! The expectation is deducible from spec text alone: the members resolve
//! (§5.6), render on the live view (§12.2), and — under a `restrict` member
//! policy — keep their targets undeletable while the membership exists (§21.1).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

/// `add_doc` inserts a doc whose `reviewers` set-of-ref is supplied by the
/// `@reviewers` call parameter (§8.3 inference), under a `restrict` member
/// policy so §21.1 must see the inserted memberships.
const APP: &str = r##"{
  format: 1
  name: setref-insert-param-initializer
  suite: scenario
  spec: ["#state-model", "§5.5", "§5.6", "#deletion", "§21.1", "§8.3"]
  package: {
    $liasse: 1
    $app: "t.srpi@1.0.0"
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
            add_doc: ".docs + { id: @id, reviewers: @reviewers }"
            del_account: ".accounts - @id"
          }
        }
      }
    }
    $data: {
      accounts: {
        a1: { name: "A" }
        a2: { name: "B" }
      }
    }
  }
  steps: STEPS
}"##;

fn run_app(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("setref-insert-param-initializer"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("setref-insert-param-initializer"), SuiteKind::Red, &case)
}

/// The set-of-ref initializer parameter decodes to the target key type, so the
/// insert commits (§5.6), the memberships render on the live view (§12.2), and
/// the `restrict` policy keeps a reviewed account undeletable (§21.1) — proof
/// the inserted members are ref-valued and planner-visible, not stranded text.
#[test]
fn insert_with_setref_param_initializer_loads_commits_and_is_planner_visible() {
    let steps = r##"[
      { call: "public.docs.add_doc", args: { id: "d1", reviewers: ["a1", "a2"] },
        expect: { outcome: ok } }
      { watch: "public.docs", id: "w0",
        expect_init: { value: [ { id: "d1", reviewers: ["a1", "a2"] } ] } }
      { call: "public.docs.del_account", args: { id: "a1" },
        expect: { outcome: rejected, violates: ["§21.1"] } }
      { expect_view: { watch: "w0", value: [ { id: "d1", reviewers: ["a1", "a2"] } ] } }
    ]"##;
    let result = run_app(steps);
    for (index, step) in result.steps.iter().enumerate() {
        assert!(
            step.result.is_pass(),
            "step {index} ({}) did not pass: {:?}",
            step.action,
            step.result
        );
    }
}
