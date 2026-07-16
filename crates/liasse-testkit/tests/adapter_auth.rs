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

/// A package whose `member` role is gated by a **session-backed** authenticator
/// (§11.3): `$verify` runs a host verifier namespace (declared by the `hosts`
/// block's `tokens` table), `$session` resolves the live session row, and
/// `$actor` dereferences that row's account ref (§5.6). `alice` is enabled with a
/// live session; `mallory` is disabled with a live session (resolves as an actor
/// but is no member).
const SESSION_APP: &str = r##"{
  format: 1
  name: adapter-auth-session
  suite: scenario
  spec: ["#authentication"]
  package: {
    $liasse: 1
    $app: "t.adapter_auth_session@1.0.0"
    $requires: { token: "test.token@1" }
    $model: {
      accounts: { $key: "id", id: "text", name: "text", enabled: "bool = true" }
      sessions: {
        $key: "id"
        id: "text"
        account: { $ref: "/accounts" }
        expires_at: "timestamp"
        revoked: "bool = false"
      }
      notes: { $key: "id", id: "uuid = uuid()", author: { $ref: "/accounts" }, body: "text" }
      $mut: {
        add_note: [
          "n = .notes + { author: $actor, body: @body }"
          "return n { author, body }"
        ]
      }
      $auth: {
        session: {
          $credential: "text"
          $verify: "token.verify($credential)"
          $session: "/sessions[$proof.session]"
          $actor: "/accounts[$session.account]"
          $check: ["$proof.auth == $auth_name", "!$session.revoked"]
        }
      }
      $roles: {
        member: {
          $auth: "session"
          $members: ".accounts[:a | a.enabled]"
          notes: { $mut: { add: ".add_note" } }
        }
      }
    }
    $data: {
      accounts: { alice: { name: "alice" }, mallory: { name: "mallory", enabled: false } }
      sessions: {
        s_alice: { account: "alice", expires_at: "1769904000000000" }
        s_mallory: { account: "mallory", expires_at: "1769904000000000" }
      }
    }
  }
  hosts: {
    token: {
      $namespace: "test.token@1"
      tokens: {
        "tok-alice": { auth: "session", session: "s_alice" }
        "tok-mallory": { auth: "session", session: "s_mallory" }
        "tok-wrongauth": { auth: "other", session: "s_alice" }
      }
    }
  }
  steps: STEPS
}"##;

/// A package whose verifier is the documented behavioral `authsim` that splits a
/// credential at the first `:` into `{ auth, account }` (see
/// `tests/12-clients-live-views/NOTES.md`); the `hosts` block declares the
/// verifier function with no static `accepts` table.
const SPLIT_APP: &str = r##"{
  format: 1
  name: adapter-auth-split
  suite: scenario
  spec: ["#authentication", "#host-namespaces"]
  package: {
    $liasse: 1
    $app: "t.adapter_auth_split@1.0.0"
    $requires: { authsim: "test.authsim@1" }
    $model: {
      accounts: { $key: "id", id: "text", enabled: "bool = true" }
      notes: { $key: "id", id: "text", body: "text" }
      $mut: { add_note: [ "n = .notes + { id: @id, body: @body }", "return n { id, body }" ] }
      $auth: {
        token: {
          $credential: "text"
          $verify: "authsim.verify($credential)"
          $actor: "/accounts[$proof.account]"
          $check: "$proof.auth == $auth_name"
        }
      }
      $roles: {
        member: {
          $auth: "token"
          $members: ".accounts[:a | a.enabled]"
          notes: { $mut: { add: ".add_note" } }
        }
      }
    }
    $data: { accounts: { alice: {} } }
  }
  hosts: {
    namespaces: {
      authsim: {
        contract: "test.authsim@1"
        functions: {
          verify: { effect: "verifier", signature: "(text) -> { auth: text, account: text }" }
        }
      }
    }
  }
  steps: STEPS
}"##;

/// Build a case from `template` with its `steps` array spliced in.
fn case_from(template: &str, steps: &str) -> Case {
    let text = template.replace("STEPS", steps);
    Case::from_hjson(&text, Path::new("<adapter-auth>"), &BTreeSet::new()).expect("case parses")
}

/// Run a scenario built from `template` against the real adapter.
fn run_from(template: &str, steps: &str) -> CaseResult {
    let case = case_from(template, steps);
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("adapter-auth"), SuiteKind::Common, &case)
}

/// Run a scenario built from [`ROLE_APP`] against the real adapter.
fn run(steps: &str) -> CaseResult {
    run_from(ROLE_APP, steps)
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

#[test]
fn session_backed_authenticator_binds_actor_from_session_row() {
    // §11.3: the host verifier turns `tok-alice` into a proof naming session
    // `s_alice`; `$session` resolves that row and `$actor` dereferences its
    // account ref (§5.6) to `alice`. The role call commits with `$actor` bound,
    // so the note's projected author ref is the resolved account key. The
    // `authenticate` step names no role: the adapter infers the one accepting
    // `session` (§11.4).
    let result = run_from(
        SESSION_APP,
        r##"[
          { connect: "c1", authenticate: { auth: "session", credential: "tok-alice" } }
          { call: "member.notes.add", args: { body: "hello" }, on: "c1",
            expect: { outcome: ok, value: { author: "alice", body: "hello" } } }
        ]"##,
    );
    assert_step(&result, 1, Outcome::Ok);
}

#[test]
fn per_request_auth_selection_admits_a_role_call() {
    // §11.4: with no context bound at connect, a per-request `auth` selection on
    // the call verifies and admits it — the credential is re-verified against
    // committed state at the request, not cached.
    let result = run_from(
        SESSION_APP,
        r##"[
          { connect: "c1" }
          { call: "member.notes.add", args: { body: "direct" }, on: "c1",
            auth: { auth: "session", credential: "tok-alice" },
            expect: { outcome: ok, value: { author: "alice", body: "direct" } } }
        ]"##,
    );
    assert_step(&result, 1, Outcome::Ok);
}

#[test]
fn session_actor_disabled_account_is_no_member() {
    // §10.3: `tok-mallory` resolves the actor `mallory`, but the disabled account
    // does not occur in `$members`, so the role grants nothing — denied even
    // though the session and actor resolve.
    let result = run_from(
        SESSION_APP,
        r##"[
          { connect: "c1", authenticate: { auth: "session", credential: "tok-mallory" } }
          { call: "member.notes.add", args: { body: "nope" }, on: "c1",
            expect: { outcome: denied, violates: ["#interfaces"] } }
        ]"##,
    );
    assert_step(&result, 1, Outcome::Denied);
}

#[test]
fn proof_bound_to_a_different_authenticator_is_denied() {
    // §11.4: `tok-wrongauth`'s proof carries `auth: "other"`, not the selected
    // `session`, so the `$proof.auth == $auth_name` binding fails and the request
    // is denied — a proof minted elsewhere cannot be replayed here.
    let result = run_from(
        SESSION_APP,
        r##"[
          { connect: "c1", authenticate: { auth: "session", credential: "tok-wrongauth" } }
          { call: "member.notes.add", args: { body: "replay" }, on: "c1",
            expect: { outcome: denied, violates: ["#authentication"] } }
        ]"##,
    );
    assert_step(&result, 1, Outcome::Denied);
}

#[test]
fn behavioral_split_verifier_resolves_the_account() {
    // The documented `authsim` verifier splits `token:alice` at the first `:`
    // into `{ auth: "token", account: "alice" }`; the stateless authenticator
    // resolves `$actor` from that account claim and the role call commits.
    let result = run_from(
        SPLIT_APP,
        r##"[
          { connect: "c1", authenticate: { auth: "token", credential: "token:alice" } }
          { call: "member.notes.add", args: { id: "n1", body: "hi" }, on: "c1",
            expect: { outcome: ok, value: { id: "n1", body: "hi" } } }
        ]"##,
    );
    assert_step(&result, 1, Outcome::Ok);
}

#[test]
fn split_verifier_rejects_a_credential_without_a_colon() {
    // A credential with no `:` does not verify under the behavioral splitter, so
    // the role surface stays out of reach — denied, never admitted.
    let result = run_from(
        SPLIT_APP,
        r##"[
          { connect: "c1", authenticate: { auth: "token", credential: "no-colon" } }
          { call: "member.notes.add", args: { id: "n2", body: "x" }, on: "c1",
            expect: { outcome: denied, violates: ["#interfaces"] } }
        ]"##,
    );
    assert_step(&result, 1, Outcome::Denied);
}