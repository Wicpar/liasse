//! RED-TEAM probe (regression guard): a multi-hop §20.1 upgrade route (two
//! declared deltas that must COMPOSE) must not be silently committed after running
//! only the first hop. Before the fix, `connected_delta_path` admitted such a
//! route and `build_migrated` committed the single active-source hop.
//!
//! §20.1 route resolution: "each key's delta bridges that version to the next
//! declared key, and the greatest key's delta bridges to the package's own
//! version." + "composition of a multi-step route is exactly the declared chain,
//! never an implementation option. The runtime MUST NOT synthesize an undeclared
//! intermediate version."
//!
//! The active instance is 1.0.0; the target 3.0.0 declares TWO deltas:
//!   "1.0.0": bridges 1.0.0 -> 2.0.0 (value += 10)
//!   "2.0.0": bridges 2.0.0 -> 3.0.0 (value += 100)
//! The spec route from 1.0.0 to 3.0.0 walks BOTH: value 0 -> 10 -> 110.
//!
//! Multi-step chain walking is a documented unbuilt hole (SPEC-ISSUES #22). The
//! runtime therefore cannot legally produce the composed value 110. The only
//! spec-legal action left is fail-closed refusal (reject, keep 1.0.0 active,
//! §9.4/Annex E.9) — exactly as `20/sequence-composition-off-lineage-rejected`
//! does. Committing the single-hop result (value 10) is committing an UNDECLARED
//! composition (the 2.0.0 intermediate state stamped as the 3.0.0 version), which
//! §20.1 forbids.
//!
//! THE FINDING (now fixed): `connected_delta_path` returned true because the
//! active source 1.0.0 has a declared delta, WITHOUT checking that a declared key
//! (2.0.0) lies strictly between the active version and the target — a genuine
//! two-hop route. `build_migrated` then ran ONLY `program("1.0.0")` and committed
//! value = 10, silently losing the second declared hop. The fix rejects any route
//! with a declared key strictly between the active and target versions.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

// The 1.0.0 base package (loaded as the root), one counter seeded at value 0.
const APP: &str = r##"{
  format: 1
  name: migration-multi-hop-partial-commit
  suite: scenario
  spec: ["#evolution", "§20.1", "#loading", "§9.4"]
  package: {
    $liasse: 1
    $app: "t.mig.twohop@1.0.0"
    $model: {
      counters: { $key: "id", id: "text", value: "int" }
      $public: { counters: { $view: ".counters { id, value }" } }
    }
    $data: { counters: { c1: { value: "0" } } }
  }
  steps: STEPS
}"##;

// The inline 3.0.0 target with TWO declared deltas forming a two-hop route.
const V3: &str = r##"{
  $liasse: 1
  $app: "t.mig.twohop@3.0.0"
  $model: {
    counters: { $key: "id", id: "text", value: "int" }
    $migrations: {
      "1.0.0": [ ".counters = $old.counters { id, value: .value + 10 }" ]
      "2.0.0": [ ".counters = $old.counters { id, value: .value + 100 }" ]
    }
    $public: { counters: { $view: ".counters { id, value }" } }
  }
}"##;

// A LEGITIMATE single-hop 3.0.0 target: one lone `$migrations` key "1.0.0" whose
// delta bridges 1.0.0 directly to the package's own version 3.0.0 (§20.1: the
// greatest — here only — key bridges to the package version). No declared key sits
// strictly between 1.0.0 and 3.0.0, so this is a single hop the runtime CAN run
// and MUST commit (value 0 -> 10). It is the control that proves the multi-hop
// guard does not over-reject a legitimate active-source delta spanning two majors.
const V3_SINGLE: &str = r##"{
  $liasse: 1
  $app: "t.mig.twohop@3.0.0"
  $model: {
    counters: { $key: "id", id: "text", value: "int" }
    $migrations: {
      "1.0.0": [ ".counters = $old.counters { id, value: .value + 10 }" ]
    }
    $public: { counters: { $view: ".counters { id, value }" } }
  }
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<migration-multi-hop-partial-commit>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("20-evolution-migrations"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult) {
    for (index, step) in result.steps.iter().enumerate() {
        assert!(
            step.result.is_pass(),
            "step {index} did not pass: observed={:?} result={:?}",
            step.observed,
            step.result
        );
    }
}

/// THE FINDING (regression guard): the spec-legal outcome for an un-composable
/// multi-hop route is fail-closed refusal — reject the update and keep 1.0.0 active
/// with value "0" (§9.4/Annex E.9). Before the fix the engine committed the
/// single-hop partial result (value "10"); now the route is rejected and the value
/// stays "0".
#[test]
fn finding_multi_hop_route_must_not_partial_commit() {
    let steps = r##"[
      { host_load: { package: V3 },
        expect: { outcome: rejected, violates: ["#evolution", "§20.1"] } }
      { watch: "public.counters", id: "w1",
        expect_init: { value: [ { id: "c1", value: "0" } ] } }
    ]"##
    .replace("V3", V3);
    assert_all_pass(&run(&steps));
}

/// SELF-RED-TEAM guard: the multi-hop refusal must NOT over-reject a legitimate
/// single hop whose active source has a declared delta. A lone `$migrations` key
/// "1.0.0" in a `3.0.0` package bridges 1.0.0 directly to 3.0.0 in ONE hop (no
/// declared key strictly between), so the update MUST commit the composed single
/// hop (value 0 -> 10). This fails if the fix were too blunt (rejecting every
/// active-source delta whenever the versions span more than one minor/patch).
#[test]
fn guard_single_declared_hop_across_two_majors_commits() {
    let steps = r##"[
      { host_load: { package: V3_SINGLE }, expect: { outcome: ok } }
      { watch: "public.counters", id: "w1",
        expect_init: { value: [ { id: "c1", value: "10" } ] } }
    ]"##
    .replace("V3_SINGLE", V3_SINGLE);
    assert_all_pass(&run(&steps));
}
