//! The scenario adapter's §11/§12 client host-operation steps —
//! `operation_status`, `manifest`, `resume`, and the standalone `authenticate`
//! step — driven against the real surface host over an in-memory store.
//!
//! Every expectation is re-derived from SPEC.md, not from observing the engine:
//!
//! - §12.3: a retained operation reports `committed` when queried by its exact
//!   high-entropy identifier — from any connection, because the identifier is the
//!   capability — and `unknown` for any other identifier, leaking nothing.
//! - §12.1: `manifest` lists the surfaces granted to the connection's context; an
//!   unauthenticated context is granted exactly the package's public surfaces.
//! - §12.2: resuming a subscription reconstructs the current authorized view.
//! - §11.4: a role accepts a fixed authenticator set; a selection the targeted
//!   role does not accept is `denied` before the credential is even inspected,
//!   while an accepted selection binds.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, Outcome, ScenarioAdapter, SuiteKind};

/// A minimal task app with a public surface (a view and an `add` call), enough to
/// submit an operation, list a manifest, and resume a subscription.
const TASKS: &str = r##"{
  format: 1
  name: adapter-host-ops
  suite: scenario
  spec: ["#clients"]
  package: {
    $liasse: 1
    $app: "t.hostops@1.0.0"
    $model: {
      tasks: { $key: "id", id: "uuid = uuid()", title: "text" }
      $mut: { add: [ "t = .tasks + { title: @title }", "return t { id, title }" ] }
      index: { $view: ".tasks { id, title, $sort: [title] }" }
      $public: {
        tasks: { $view: ".index", $mut: { add: ".add" } }
      }
    }
  }
  steps: STEPS
}"##;

/// A role-gated app whose `member` role accepts only the `token` authenticator,
/// though the package also declares `apikey`. Identity verifiers keep the case
/// host-free (§11.4: the accepted set is decided before the credential is read).
const AUTH: &str = r##"{
  format: 1
  name: adapter-host-ops-auth
  suite: scenario
  spec: ["#authentication"]
  package: {
    $liasse: 1
    $app: "t.hostopsauth@1.0.0"
    $model: {
      accounts: { $key: "id", id: "text" }
      notes: { $key: "id", id: "text", body: "text" }
      $mut: { add_note: [ "n = .notes + { id: @id, body: @body }", "return n { id, body }" ] }
      $auth: {
        token: { $credential: "text", $verify: "$credential", $actor: "/accounts[$proof]" }
        apikey: { $credential: "text", $verify: "$credential", $actor: "/accounts[$proof]" }
      }
      $roles: {
        member: {
          $auth: "token"
          $members: ".accounts"
          notes: { $view: ".notes { id, body }", $mut: { add: ".add_note" } }
        }
      }
    }
    $data: { accounts: { alice: {} } }
  }
  steps: STEPS
}"##;

fn run(template: &str, steps: &str) -> CaseResult {
    let text = template.replace("STEPS", steps);
    // These are chapter-local step keys (§11–§12); document them for the loader.
    let allowed: BTreeSet<String> =
        ["operation_status", "manifest", "resume", "authenticate"].into_iter().map(ToOwned::to_owned).collect();
    let case = Case::from_hjson(&text, Path::new("<adapter-host-ops>"), &allowed).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("adapter-host-ops"), SuiteKind::Common, &case)
}

fn assert_pass(result: &CaseResult, index: usize, expected: Outcome) {
    let step = result.steps.get(index).unwrap_or_else(|| panic!("no step {index}: {:?}", result.steps));
    assert!(step.result.is_pass(), "step {index} did not pass: {:?}", result.steps);
    assert_eq!(step.observed, Some(expected), "step {index} observed the wrong outcome: {:?}", result.steps);
}

#[test]
fn operation_status_identifier_is_the_capability() {
    // §12.3: the exact identifier addresses the committed record from a *different*
    // connection (the identifier is the capability); a guessed identifier addresses
    // no record and reports `unknown`, revealing nothing. A driver that ignored the
    // identifier scope would leak the committed status for the guess, failing the
    // `unknown` matcher; one that could not reconstruct the key would report
    // `unknown` for the real identifier, failing the `committed` matcher.
    let result = run(
        TASKS,
        r##"[
          { connect: "c1" }
          { call: "public.tasks.add", args: { title: "a" }, on: "c1",
            operation_id: "op-known-1d4b9a6e30c7f582",
            expect: { outcome: ok, completion: committed } }
          { connect: "c2" }
          { operation_status: { id: "op-guess-4c19e7a2f58d03b7" }, on: "c2",
            expect: { outcome: ok, value: { status: "unknown" } } }
          { operation_status: { id: "op-known-1d4b9a6e30c7f582" }, on: "c2",
            expect: { outcome: ok, value: { status: "committed", frontier: "$any", commit: "$any" } } }
        ]"##,
    );
    assert_pass(&result, 3, Outcome::Ok);
    assert_pass(&result, 4, Outcome::Ok);
}

#[test]
fn manifest_lists_public_surfaces_for_an_unauthenticated_context() {
    // §12.1: with no authenticated context, the manifest is exactly the package's
    // public surfaces. A driver that dropped the manifest or reported the wrong set
    // would fail the closed-object matcher.
    let result = run(
        TASKS,
        r##"[
          { connect: "c1" }
          { manifest: {}, on: "c1",
            expect: { outcome: ok, value: { surfaces: ["public.tasks"] } } }
        ]"##,
    );
    assert_pass(&result, 1, Outcome::Ok);
}

#[test]
fn resume_reconstructs_the_current_authorized_view() {
    // §12.2: resuming from a retained frontier yields the authorized declared view
    // at the current frontier. Two tasks committed after the original init must both
    // appear in the resumed result on a fresh connection.
    let result = run(
        TASKS,
        r##"[
          { connect: "c1" }
          { watch: "public.tasks", on: "c1", id: "w1", expect_init: { value: [] } }
          { call: "public.tasks.add", args: { title: "a" }, on: "c1", expect: { outcome: ok } }
          { call: "public.tasks.add", args: { title: "b" }, on: "c1", expect: { outcome: ok } }
          { disconnect: "c1" }
          { connect: "c2" }
          { resume: { surface: "public.tasks", from: 0, id: "w2" }, on: "c2",
            expect: { outcome: ok, value: [ { id: "$any:uuid", title: "a" }, { id: "$any:uuid", title: "b" } ] } }
        ]"##,
    );
    assert_pass(&result, 6, Outcome::Ok);
}

#[test]
fn authenticate_refuses_an_unaccepted_authenticator_and_binds_an_accepted_one() {
    // §11.4: the `member` role accepts only `token`. Selecting the package's other
    // declared authenticator `apikey` is denied even though the credential would
    // verify under it; selecting `token` binds. A driver that ignored the role's
    // accepted set would bind the `apikey` selection, failing the `denied` matcher.
    let result = run(
        AUTH,
        r##"[
          { connect: "c1" }
          { authenticate: { role: "member", auth: "apikey", credential: "alice" }, on: "c1",
            expect: { outcome: denied, violates: ["#authentication"] } }
          { authenticate: { role: "member", auth: "token", credential: "alice" }, on: "c1",
            expect: { outcome: ok } }
        ]"##,
    );
    assert_pass(&result, 1, Outcome::Denied);
    assert_pass(&result, 2, Outcome::Ok);
}
