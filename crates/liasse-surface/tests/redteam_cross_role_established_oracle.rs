//! RED TEAM — §10.4 role-existence enumeration oracle through the `established`
//! predicate: a bound connection context for ONE role is treated as established
//! authority over EVERY OTHER role.
//!
//! §10.4 (verbatim MUST): "The denial MUST NOT reveal whether a surface of the
//! named address exists: for a fixed caller and authentication context, the
//! observable denial — its class and any diagnostic code — for a name that does
//! not exist MUST be identical to that for a name that exists but is not granted
//! to that caller." The exception is narrow and target-scoped: "Membership- or
//! existence-specific diagnostics are permitted only toward a caller that has
//! already established authority over THE TARGET."
//!
//! The WAVE-1 fix (`host/call.rs::hide_unenumerable_denial`) collapses every
//! pre-authority denial over a role target to the uniform `Unresolved` — but ONLY
//! when the caller is not `established`. The impl derives that predicate as
//! `established = call.auth().is_none()` (`resolve_call`, line ~230): a request that
//! authorizes from a bound connection context rather than a per-request `auth`
//! selection is deemed established. The connection context, however, is stored by
//! [`SurfaceHost::authenticate`] as a bare `AuthSelection` under a context LABEL
//! with NO record of the role it was authenticated against (`connection.rs`,
//! `set_context`; `verify_selection` explicitly does not tie the context to a role).
//!
//! Consequence: a caller who authenticates ONCE to role `alpha` (obtaining a bound
//! "default" context) is thereafter `established = true` for a probe of ANY role —
//! including a role `beta` it never authenticated to and holds no authority over.
//! Probing `beta` with the bound context fails at authenticator ACCEPTANCE (beta
//! does not accept alpha's authenticator), and because `established` is true that
//! precise `AuthenticatorNotAccepted` reason is NOT hidden — while a nonexistent
//! role `ghost` denies `Unresolved`. The two differ in diagnostic code, so the
//! caller learns `beta` is a real role: exactly the oracle §10.4 forbids, and
//! exactly the one the WAVE-1 fix closed for the per-request-`auth` probe.
//!
//! Root cause: `crates/liasse-surface/src/host/call.rs`, `resolve_call` —
//! `let established = call.auth().is_none();` (and the identical
//! `let established = inline.is_none();` in `resolve_view`) conflates "authorizes
//! from a bound context" with "has established authority over THIS target role".
//! The bound context proves prior authority over whatever role `authenticate` was
//! called with, not over the role now being addressed.
//!
//! Expectations are deducible from SPEC.md §10.4 alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_surface::{
    Authenticate, AuthResult, AuthSelection, CallBinding, Credential, DenialReason, Engine,
    Precision, Role, RowSource, SessionAuthenticator, SessionSource, Subscription, SurfaceBinding,
    SurfaceCall, SurfaceHost, SurfaceOutcome, SurfaceRouter, SurfaceRouterBuilder, SurfaceWatch,
    ViewBinding, VirtualClock,
};
use liasse_store::MemoryStore;
use support::{address, args, store, text, TokenVerifier, NOW};

/// A two-role application: role `alpha` accepts the `token` authenticator, role
/// `beta` accepts ONLY the `api` authenticator. Both roles exist; a caller holding
/// a `token` credential can authenticate to `alpha` but can never be accepted by
/// `beta`. `alice` is an enabled account (a member of both `$members` views) with a
/// live session `s_alice`.
const TWO_ROLE_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.tworole@1.0.0"
  "$model": {
    "accounts": { "$key": "id", "id": "text", "enabled": "bool = true" }
    "sessions": {
      "$key": "id"
      "id": "text"
      "account": "text"
      "expires_at": "timestamp"
      "revoked": "bool = false"
    }
    "tasks": {
      "$key": "id"
      "id": "uuid = uuid()"
      "title": "text"
      "owner": "text = 'anon'"
    }
    "index": { "$view": ".tasks { id, title, $sort: [title] }" }
    "sessions_view": { "$view": ".sessions { id, account, expires_at, revoked }" }
    "accounts_view": { "$view": ".accounts { id, enabled }" }
    "members_view": { "$view": ".accounts[:a | a.enabled] { id }" }
    "$mut": {
      "add": ".tasks + { title: @title }"
      "rename({ title: text })": ".tasks[@id].title = @title"
    }
    "$auth": {
      "token": {
        "$credential": "text"
        "$verify": "token.verify($credential)"
        "$session": "/sessions[$proof.session]"
        "$actor": "/accounts[$session.account]"
        "$check": "$proof.auth == $auth_name"
      }
      "api": {
        "$credential": "text"
        "$verify": "token.verify($credential)"
        "$actor": "/accounts[$proof.account]"
        "$check": "$proof.auth == $auth_name"
      }
    }
    "$public": {
      "tasks": { "$view": ".index", "$mut": { "add": ".add" } }
    }
    "$roles": {
      "alpha": {
        "$auth": "token"
        "$members": ".members_view"
        "tasks": { "$view": ".index", "$mut": { "complete": ".rename" } }
      }
      "beta": {
        "$auth": "api"
        "$members": ".members_view"
        "tasks": { "$view": ".index", "$mut": { "complete": ".rename" } }
      }
    }
  }
  "$data": {
    "accounts": { "alice": { } }
    "sessions": {
      "s_alice": { "account": "alice", "expires_at": 2000000000000000 }
    }
  }
}"#;

/// Build the two-role host: public `tasks`, a session `token` authenticator, a
/// stateless `api` authenticator, and the two roles `alpha` (accepts `token`) and
/// `beta` (accepts `api`), each granting a `tasks` surface.
fn two_role_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = match Engine::load(store("tworole"), TWO_ROLE_APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    };
    let router = two_role_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn two_role_router(model: &liasse_model::Model) -> SurfaceRouter {
    let public_tasks = SurfaceBinding::new()
        .with_view(ViewBinding::new("index"))
        .with_call("add", CallBinding::root("add", ["title".to_owned()]));
    let role_tasks = || {
        SurfaceBinding::new()
            .with_view(ViewBinding::new("index"))
            .with_call("complete", CallBinding::root("rename", ["id".to_owned(), "title".to_owned()]))
    };
    let token = SessionAuthenticator::session(
        "token",
        Box::new(TokenVerifier::new("token", true)),
        SessionSource::new(RowSource::new("sessions_view", "id"), "account", "expires_at", "revoked"),
        RowSource::new("accounts_view", "id"),
    );
    let api = SessionAuthenticator::stateless(
        "api",
        Box::new(TokenVerifier::new("api", false)),
        RowSource::new("accounts_view", "id"),
    );
    let alpha = Role::new("alpha", ["token".to_owned()], RowSource::new("members_view", "id"));
    let beta = Role::new("beta", ["api".to_owned()], RowSource::new("members_view", "id"));

    SurfaceRouterBuilder::new()
        .public_surface("tasks", public_tasks)
        .authenticator(Box::new(token))
        .authenticator(Box::new(api))
        .role(alpha, [("tasks".to_owned(), role_tasks())])
        .role(beta, [("tasks".to_owned(), role_tasks())])
        .build(model)
        .expect("router validates against the model")
}

/// The denial reason of a call outcome, or a panic naming what happened.
fn denial_reason(outcome: &SurfaceOutcome) -> DenialReason {
    match outcome.denial() {
        Some(denial) => denial.reason(),
        None => panic!("expected a denial, got {outcome:?}"),
    }
}

/// Authenticate connection `c1` to role `alpha` with the live `token` credential
/// `s_alice`, binding the default connection context. This is the ONLY role the
/// caller ever establishes authority over.
fn host_established_on_alpha() -> SurfaceHost<MemoryStore> {
    let mut host = two_role_host();
    host.connect("c1").unwrap();
    let request = Authenticate::new("alpha", AuthSelection::new("token", Credential::new(text("s_alice"))));
    assert!(
        matches!(host.authenticate("c1", &request).expect("authenticate"), AuthResult::Bound),
        "the caller establishes authority over `alpha` only",
    );
    host
}

/// A bound-context call (no per-request `auth`) to `<role>.tasks.complete`,
/// returning the denial reason observed. Uses the connection's stored context, so
/// `call.auth().is_none()` and the impl treats the caller as `established`.
fn probe_bound(host: &mut SurfaceHost<MemoryStore>, role: &str) -> DenialReason {
    let call = SurfaceCall::new(
        address(&format!("{role}.tasks.complete")),
        args([("id", text("x")), ("title", text("y"))]),
    );
    denial_reason(&host.call("c1", &call).expect("call runs"))
}

/// FINDING (§10.4) — a bound context established for `alpha` leaks the existence of
/// an unrelated role `beta` the caller never authenticated to.
///
/// The single fixed caller (bound "default" context = a `token` credential
/// established against `alpha`) probes two addresses whose only difference is the
/// role name: `beta` (a real role that does NOT accept `token`) vs `ghost` (no such
/// role). The caller has established NO authority over `beta` — its bound context is
/// authority over `alpha` alone — so §10.4 requires the two denials to be identical
/// in class AND diagnostic code. The impl denies `beta` with
/// `AuthenticatorNotAccepted` (its `established` predicate is true, so the reason is
/// not hidden) and `ghost` with `Unresolved`: an oracle revealing `beta` is real.
#[test]
fn bound_context_for_one_role_does_not_leak_another_roles_existence() {
    let mut host = host_established_on_alpha();
    let beta = probe_bound(&mut host, "beta");
    let ghost = probe_bound(&mut host, "ghost");

    assert_eq!(
        beta, ghost,
        "§10.4: a caller established over `alpha` but not `beta` must see the \
         IDENTICAL diagnostic code for the existing-but-ungranted role `beta` and \
         the nonexistent role `ghost`, or `beta`'s existence leaks — \
         beta={beta:?} ghost={ghost:?}",
    );
}

/// The denial reason of a bound-context `watch` of `<role>.tasks`, or a panic. The
/// subscription pipeline (`resolve_view`) carries the SAME `established = inline.is_none()`
/// predicate as the `call` pipeline, so the oracle is not call-specific.
fn probe_bound_watch(host: &mut SurfaceHost<MemoryStore>, role: &str) -> DenialReason {
    let watch = SurfaceWatch::new(address(&format!("{role}.tasks")), "w1");
    match host.watch("c1", &watch).expect("watch runs") {
        Subscription::Denied(denial) => denial.reason(),
        other => panic!("expected a denial for role {role}, got {other:?}"),
    }
}

/// FINDING (§10.4) — the SAME oracle on the `watch` (subscription) pipeline.
///
/// A caller established over `alpha` opens a bound-context subscription on `beta`'s
/// view (`beta.tasks`) vs a nonexistent `ghost.tasks`. `resolve_view` sets
/// `established = inline.is_none()` (true for a bound context) exactly as
/// `resolve_call` does, so `beta` denies `AuthenticatorNotAccepted` while `ghost`
/// denies `Unresolved`: §10.4's identical-code requirement is violated on the
/// subscription path too, proving the leak spans call AND view/watch/resume.
#[test]
fn bound_context_watch_leaks_another_roles_existence() {
    let mut host = host_established_on_alpha();
    let beta = probe_bound_watch(&mut host, "beta");
    let ghost = probe_bound_watch(&mut host, "ghost");

    assert_eq!(
        beta, ghost,
        "§10.4: a bound-context subscription probe of an existing-but-ungranted role \
         `beta` and a nonexistent role `ghost` must deny with the IDENTICAL diagnostic \
         code — beta={beta:?} ghost={ghost:?}",
    );
}

/// PASSING CONTROL — the SAME probe carried out with a per-request `auth` selection
/// (no bound context, `established = false`) is correctly uniform.
///
/// Here the caller attaches the credential inline to each call instead of relying on
/// a stored context, so the WAVE-1 fix collapses every pre-authority reason to
/// `Unresolved`. Both `beta` and `ghost` therefore deny `Unresolved`: the oracle is
/// closed on the per-request path and the leak above is specific to the
/// bound-context `established` predicate.
#[test]
fn per_request_auth_probe_is_uniform_control() {
    let mut host = two_role_host();
    host.connect("c1").unwrap();
    let probe = |host: &mut SurfaceHost<MemoryStore>, role: &str| -> DenialReason {
        let call = SurfaceCall::new(
            address(&format!("{role}.tasks.complete")),
            args([("id", text("x")), ("title", text("y"))]),
        )
        .with_auth(AuthSelection::new("token", Credential::new(text("s_alice"))));
        denial_reason(&host.call("c1", &call).expect("call runs"))
    };
    let beta = probe(&mut host, "beta");
    let ghost = probe(&mut host, "ghost");

    assert_eq!(beta, DenialReason::Unresolved, "a fresh per-request probe of `beta` denies `unresolved`");
    assert_eq!(
        beta, ghost,
        "control: a per-request-auth probe sees the uniform denial for an existing \
         and a nonexistent role — beta={beta:?} ghost={ghost:?}",
    );
}
