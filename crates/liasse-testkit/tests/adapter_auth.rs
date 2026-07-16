//! The scenario adapter's §11 authentication wiring, driven against the real
//! runtime + surface stack over an in-memory store.
//!
//! These lock in the host-free authentication path the adapter reconstructs from
//! a package's `$auth`/`$roles`: a `$verify: "$credential"` authenticator whose
//! `$actor` selects an account, and a role whose `$members` is an inline
//! row-stream filter. Every expected outcome is deducible from SPEC.md alone:
//!
//! - §10.3/§11.4: a resolved actor that occurs in `$members` may use the role's
//!   surfaces; one that does not is denied — membership is re-evaluated at
//!   admission, not cached at connect.
//! - §10.1/§11.4: an unauthenticated connection cannot reach a role surface.
//!
//! They are not tautological — the admit/deny split is fixed by the spec's
//! membership rule, not by observing the engine.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, Outcome, ScenarioAdapter, SuiteKind};

/// A package exposing a `member` role gated on an enabled-account membership
/// filter, with a single root mutation reachable through the role surface.
/// `alice` is enabled (a member); `mallory` is disabled (not a member).
const ROLE_APP: &str = r##"{
  format: 1
  name: adapter-auth-membership
  suite: scenario
  spec: ["#interfaces"]
  package: {
    $liasse: 1
    $app: "t.adapter_auth@1.0.0"
    $model: {
      accounts: { $key: "id", id: "text", enabled: "bool = true" }
      notes: { $key: "id", id: "text", body: "text" }
      $mut: {
        add_note: [
          "n = .notes + { id: @id, body: @body }"
          "return n { id, body }"
        ]
      }
      $auth: {
        token: {
          $credential: "text"
          $verify: "$credential"
          $actor: "/accounts[$proof]"
        }
      }
      $roles: {
        member: {
          $auth: "token"
          $members: ".accounts[:a | a.enabled]"
          notes: {
            $view: ".notes { id, body }"
            $mut: { add: ".add_note" }
          }
        }
      }
    }
    $data: {
      accounts: { alice: {}, mallory: { enabled: false } }
    }
  }
  steps: STEPS
}"##;

/// Build a case from [`ROLE_APP`] with its `steps` array spliced in.
fn case_with(steps: &str) -> Case {
    let text = ROLE_APP.replace("STEPS", steps);
    Case::from_hjson(&text, Path::new("<adapter-auth>"), &BTreeSet::new()).expect("case parses")
}

/// Run a scenario built from [`ROLE_APP`] against the real adapter.
fn run(steps: &str) -> CaseResult {
    let case = case_with(steps);
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("adapter-auth"), SuiteKind::Common, &case)
}

/// Assert that step `index` ran, its expectation held, and it observed
/// `expected` — so the assertion fails loudly whether the adapter reported the
/// wrong outcome or skipped the step (a load/transport fault).
fn assert_step(result: &CaseResult, index: usize, expected: Outcome) {
    let step = result.steps.get(index).unwrap_or_else(|| panic!("no step {index}: {:?}", result.steps));
    assert!(step.result.is_pass(), "step {index} did not pass: {:?}", result.steps);
    assert_eq!(step.observed, Some(expected), "step {index} observed the wrong outcome");
}

#[test]
fn member_actor_reaches_role_surface() {
    // §10.3/§11.4: alice is enabled, so her actor occurs in `$members`; the role
    // call is admitted and commits.
    let result = run(
        r##"[
          { connect: "c1", authenticate: { role: "member", auth: "token", credential: "alice" } }
          { call: "member.notes.add", args: { id: "n1", body: "hello" }, on: "c1",
            expect: { outcome: ok, value: { id: "n1", body: "hello" } } }
        ]"##,
    );
    assert_step(&result, 1, Outcome::Ok);
}

#[test]
fn non_member_actor_denied_at_admission() {
    // §10.3: mallory's actor row resolves but is disabled, so it does not occur
    // in `$members`; the role grants nothing and the call is denied.
    let result = run(
        r##"[
          { connect: "c1", authenticate: { role: "member", auth: "token", credential: "mallory" } }
          { call: "member.notes.add", args: { id: "n2", body: "nope" }, on: "c1",
            expect: { outcome: denied, violates: ["#interfaces"],
                      detail: "actor is not a member of the role" } }
        ]"##,
    );
    assert_step(&result, 1, Outcome::Denied);
}

#[test]
fn unauthenticated_connection_denied_at_role_surface() {
    // §11.4: a connection that never authenticated has no bound actor, so a role
    // surface is out of reach — denied, never silently admitted.
    let result = run(
        r##"[
          { connect: "c1" }
          { call: "member.notes.add", args: { id: "n3", body: "anon" }, on: "c1",
            expect: { outcome: denied, violates: ["#interfaces"],
                      detail: "a role surface requires an authenticated actor" } }
        ]"##,
    );
    assert_step(&result, 1, Outcome::Denied);
}