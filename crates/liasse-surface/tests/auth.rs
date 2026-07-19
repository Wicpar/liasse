#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §11 authentication and sessions: token issue/use/expiry, session continuation
//! and revocation, membership admission, and the authenticator-selection rules.
//! All authentication and role-admission failures are `denied`
//! (`tests/11-auth-sessions/NOTES.md` outcome conventions).

mod support;

use liasse_surface::{
    AuthResult, AuthSelection, Authenticate, Credential, DenialReason, SurfaceCall, SurfaceOutcome, Value,
};
use support::{add_task, address, args, authenticate_member, call, host, text, timestamp, FUTURE};

/// The denial reason of an `AuthResult`, or a panic naming what happened.
fn auth_denial(result: &AuthResult) -> DenialReason {
    match result {
        AuthResult::Denied(denial) => denial.reason(),
        AuthResult::Bound => panic!("expected a denial, got Bound"),
    }
}

/// The denial reason of a call outcome, or a panic.
fn call_denial(outcome: &SurfaceOutcome) -> DenialReason {
    match outcome.denial() {
        Some(denial) => denial.reason(),
        None => panic!("expected a denial, got {outcome:?}"),
    }
}

#[test]
fn authenticated_member_call_commits() {
    let mut host = host();
    host.connect("c1").unwrap();
    let id = add_task(&mut host, "c1", "chore");

    assert!(matches!(authenticate_member(&mut host, "c1", "s_alice"), AuthResult::Bound));
    let outcome = host
        .call("c1", &call("member.tasks.complete", [("id", id), ("title", text("done"))]))
        .expect("call");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "member commits: {outcome:?}");
}

#[test]
fn login_issues_immediately_usable_session() {
    // §11.5: the mutation commits before the response, so the returned token is
    // usable at once. Here the login inserts a session; a follow-up request
    // authenticates against it.
    let mut host = host();
    host.connect("c1").unwrap();
    let open = host
        .call(
            "c1",
            &SurfaceCall::new(
                address("public.login.open"),
                args([("id", text("s_new")), ("account", text("alice")), ("expires", timestamp(FUTURE))]),
            ),
        )
        .expect("login");
    assert!(matches!(open, SurfaceOutcome::Committed { .. }), "login commits: {open:?}");

    assert!(matches!(authenticate_member(&mut host, "c1", "s_new"), AuthResult::Bound), "new session authenticates");
    let id = add_task(&mut host, "c1", "task");
    let outcome = host.call("c1", &call("member.tasks.complete", [("id", id), ("title", text("x"))])).expect("call");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "the freshly-issued session admits a member call");
}

#[test]
fn expired_session_is_denied() {
    let mut host = host();
    host.connect("c1").unwrap();
    assert_eq!(auth_denial(&authenticate_member(&mut host, "c1", "s_expired")), DenialReason::SessionInvalid);
}

#[test]
fn unknown_session_is_denied() {
    let mut host = host();
    host.connect("c1").unwrap();
    assert_eq!(auth_denial(&authenticate_member(&mut host, "c1", "s_missing")), DenialReason::SessionInvalid);
}

#[test]
fn session_expiry_crosses_at_the_clock() {
    // A live session admits a call; once the virtual clock passes its expiry the
    // very next request is denied (§11.7 expiry via the engine clock).
    let mut host = host();
    host.connect("c1").unwrap();
    let id = add_task(&mut host, "c1", "t");
    assert!(matches!(authenticate_member(&mut host, "c1", "s_alice"), AuthResult::Bound));

    host.clock_mut().set(FUTURE + 1);
    let outcome = host.call("c1", &call("member.tasks.complete", [("id", id), ("title", text("x"))])).expect("call");
    assert_eq!(call_denial(&outcome), DenialReason::SessionInvalid, "an expired session denies at admission");
}

#[test]
fn revoked_session_is_denied_at_next_request() {
    let mut host = host();
    host.connect("c1").unwrap();
    let id = add_task(&mut host, "c1", "t");
    assert!(matches!(authenticate_member(&mut host, "c1", "s_alice"), AuthResult::Bound));

    let revoke = host.call("c1", &call("public.session.revoke", [("id", text("s_alice"))])).expect("revoke");
    assert!(matches!(revoke, SurfaceOutcome::Committed { .. }), "revoke commits: {revoke:?}");

    let outcome = host.call("c1", &call("member.tasks.complete", [("id", id), ("title", text("x"))])).expect("call");
    assert_eq!(call_denial(&outcome), DenialReason::SessionInvalid, "a revoked session denies");
}

#[test]
fn disabled_account_fails_role_membership() {
    // bob authenticates (the session and account resolve) but is not a member —
    // membership excludes disabled accounts (§10.3), so the call denies. Per
    // SPEC-ISSUES item 8 the denial is the uniform unresolvable-name outcome
    // (`Unresolved`), indistinguishable from a name that does not exist, so a
    // non-member cannot enumerate the role's surfaces.
    let mut host = host();
    host.connect("c1").unwrap();
    let id = add_task(&mut host, "c1", "t");
    assert!(matches!(authenticate_member(&mut host, "c1", "s_bob"), AuthResult::Bound), "bob authenticates");
    let outcome = host.call("c1", &call("member.tasks.complete", [("id", id), ("title", text("x"))])).expect("call");
    assert_eq!(call_denial(&outcome), DenialReason::Unresolved);
}

#[test]
fn role_rejects_unaccepted_authenticator() {
    // The member role accepts only `token`; a per-request `api` selection is refused
    // before any credential is resolved (§11.4). This caller is a fresh probe with no
    // established authority over `member` (no prior `authenticate`), so §10.4 makes
    // the observable denial the uniform `Unresolved` — identical to a nonexistent
    // role — rather than the acceptance-specific `AuthenticatorNotAccepted`, which
    // would let an unauthenticated caller enumerate the role catalog by wire code
    // (pinned by `redteam_denial_oracle_auth_failure`).
    let mut host = host();
    host.connect("c1").unwrap();
    let id = add_task(&mut host, "c1", "t");
    let request = call("member.tasks.complete", [("id", id), ("title", text("x"))])
        .with_auth(AuthSelection::new("api", Credential::new(text("alice"))));
    let outcome = host.call("c1", &request).expect("call");
    assert_eq!(call_denial(&outcome), DenialReason::Unresolved);
}

#[test]
fn forged_credential_fails_verification() {
    // A non-token credential fails `$verify` before any row is resolved (§11.3).
    let mut host = host();
    host.connect("c1").unwrap();
    let request = Authenticate::new("member", AuthSelection::new("token", Credential::new(Value::Bool(true))));
    let result = host.authenticate("c1", &request).expect("authenticate");
    assert_eq!(auth_denial(&result), DenialReason::Unverified);
}

#[test]
fn session_continues_across_requests() {
    // One authentication admits repeated requests (§11.8 continued access).
    let mut host = host();
    host.connect("c1").unwrap();
    let first = add_task(&mut host, "c1", "a");
    let second = add_task(&mut host, "c1", "b");
    assert!(matches!(authenticate_member(&mut host, "c1", "s_alice"), AuthResult::Bound));
    for id in [first, second] {
        let outcome = host.call("c1", &call("member.tasks.complete", [("id", id), ("title", text("done"))])).expect("call");
        assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "each request on the continued session commits");
    }
}

#[test]
fn one_connection_multiplexes_two_sessions() {
    // §11.8: a single connection holds several credentials at once, each call
    // selecting its own context.
    let mut host = host();
    host.connect("c1").unwrap();
    let id = add_task(&mut host, "c1", "t");

    assert!(matches!(authenticate_member(&mut host, "c1", "s_alice"), AuthResult::Bound));
    let work = Authenticate::new("member", AuthSelection::new("token", Credential::new(text("s_carol")))).as_context("work");
    assert!(matches!(host.authenticate("c1", &work).expect("authenticate"), AuthResult::Bound));

    // The default (alice) context admits a member call.
    let alice_call = host.call("c1", &call("member.tasks.complete", [("id", id.clone()), ("title", text("a"))])).expect("call");
    assert!(matches!(alice_call, SurfaceOutcome::Committed { .. }), "default context commits");
    // The named (carol) context admits its own member call.
    let carol_call = host
        .call("c1", &call("member.tasks.complete", [("id", id), ("title", text("c"))]).with_context("work"))
        .expect("call");
    assert!(matches!(carol_call, SurfaceOutcome::Committed { .. }), "second context commits");
}
