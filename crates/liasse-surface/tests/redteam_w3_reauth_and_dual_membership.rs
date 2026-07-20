//! RED TEAM (Wave 3) — DRY-CONFIRMATION for §10.4 establishment probes (a) and (b).
//!
//! These are the two probes where the wave-2 fix (commit 69f8242) is CORRECT; the
//! tests confirm coverage with PASSING assertions (each would FAIL under a plausible
//! regression), so they document why the probes are dry rather than inventing a bug.
//!
//! Probe (a) — RE-AUTHENTICATION overwrites the context role. A caller authenticates
//! the default context to role `alpha`, then re-authenticates the SAME default label
//! to role `gamma`. `Connection::set_context` inserts by context name, so the default
//! `context_role` is OVERWRITTEN from `alpha` to `gamma`. §10.4's exception is scoped
//! to the CURRENT established authority: after the swap the caller no longer presents
//! authority over `alpha` on that context, so a denial over `alpha` must be hidden
//! (uniform `unresolved`) exactly like a nonexistent role — no STALE establishment
//! may leak `alpha`. A regression that tracked establishment per connection (not per
//! current context role) would leak `alpha` here.
//!
//! Probe (b) — a caller that is a MEMBER of two roles through ONE authentication
//! context. `alpha` and `beta` both accept `token`; `alice` is a member of both. She
//! authenticates the default context to `alpha` and, on the SAME context, watches
//! `beta` — where she is a genuine member, so admission SERVES the subscription. There
//! is no denial to hide, so §10.4's established/hidden distinction never engages; the
//! per-named-role `established` flag being false for `beta` is immaterial because
//! membership succeeds. This confirms one-context multi-role membership is served, and
//! that establishment scoping does not wrongly DENY a legitimate member.
//!
//! Expectations are hand-derived from SPEC.md §10.4 / §11.4 / §11.8.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_surface::{
    AuthResult, AuthSelection, Authenticate, CallBinding, Credential, DenialReason, Engine,
    Precision, Role, RowSource, SessionAuthenticator, SessionSource, Subscription, SurfaceBinding,
    SurfaceHost, SurfaceRouter, SurfaceRouterBuilder, SurfaceWatch, ViewBinding, VirtualClock,
};
use liasse_store::MemoryStore;
use support::{address, store, text, TokenVerifier, NOW};

/// Three roles over one enabled account `alice`. `alpha` and `beta` both accept the
/// `token` (session) authenticator; `gamma` accepts the `api` (stateless)
/// authenticator. `alice` (enabled) is therefore a member of all three. This lets a
/// single `token` context be a member of both `alpha` and `beta` (probe b), and lets a
/// re-authentication swap the default context from `alpha` (token) to `gamma` (api),
/// whose authenticator `alpha` does NOT accept (probe a).
const THREE_ROLE_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.threerole@1.0.0"
  "$model": {
    "accounts": { "$key": "id", "id": "text", "enabled": "bool = true" }
    "sessions": {
      "$key": "id"
      "id": "text"
      "account": "text"
      "expires_at": "timestamp"
      "revoked": "bool = false"
    }
    "tasks": { "$key": "id", "id": "uuid = uuid()", "title": "text" }
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
    "$public": { "tasks": { "$view": ".index", "$mut": { "add": ".add" } } }
    "$roles": {
      "alpha": { "$auth": "token", "$members": ".members_view", "tasks": { "$view": ".index", "$mut": { "complete": ".rename" } } }
      "beta":  { "$auth": "token", "$members": ".members_view", "tasks": { "$view": ".index", "$mut": { "complete": ".rename" } } }
      "gamma": { "$auth": "api",   "$members": ".members_view", "tasks": { "$view": ".index", "$mut": { "complete": ".rename" } } }
    }
  }
  "$data": {
    "accounts": { "alice": { } }
    "sessions": { "s_alice": { "account": "alice", "expires_at": 2000000000000000 } }
  }
}"#;

fn three_role_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = match Engine::load(store("threerole"), THREE_ROLE_APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    };
    let router = three_role_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn three_role_router(model: &liasse_model::Model) -> SurfaceRouter {
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
    let beta = Role::new("beta", ["token".to_owned()], RowSource::new("members_view", "id"));
    let gamma = Role::new("gamma", ["api".to_owned()], RowSource::new("members_view", "id"));

    SurfaceRouterBuilder::new()
        .public_surface("tasks", public_tasks)
        .authenticator(Box::new(token))
        .authenticator(Box::new(api))
        .role(alpha, [("tasks".to_owned(), role_tasks())])
        .role(beta, [("tasks".to_owned(), role_tasks())])
        .role(gamma, [("tasks".to_owned(), role_tasks())])
        .build(model)
        .expect("router validates against the model")
}

fn auth(host: &mut SurfaceHost<MemoryStore>, role: &str, auth_name: &str, credential: &str) {
    let request = Authenticate::new(role, AuthSelection::new(auth_name, Credential::new(text(credential))));
    assert!(
        matches!(host.authenticate("c1", &request).expect("authenticate"), AuthResult::Bound),
        "authenticate to `{role}` binds",
    );
}

/// Watch `<role>.tasks` on the default context; report whether it opened (served) or
/// the denial reason.
fn watch(host: &mut SurfaceHost<MemoryStore>, role: &str) -> Result<(), DenialReason> {
    let watch = SurfaceWatch::new(address(&format!("{role}.tasks")), "w1");
    match host.watch("c1", &watch).expect("watch runs") {
        Subscription::Init(_) | Subscription::Window(_) => Ok(()),
        Subscription::Denied(denial) => Err(denial.reason()),
        other => panic!("unexpected subscription outcome for `{role}`: {other:?}"),
    }
}

/// DRY (probe a) — re-authenticating the default context to `gamma` overwrites the
/// recorded role, so `alpha` is no longer established on that context and its denial
/// is uniformly hidden. No STALE `alpha` establishment leaks.
#[test]
fn reauth_overwrites_establishment_no_stale_leak() {
    let mut host = three_role_host();
    host.connect("c1").unwrap();

    // Establish `alpha` on the default context; alice is a member -> served.
    auth(&mut host, "alpha", "token", "s_alice");
    assert_eq!(watch(&mut host, "alpha"), Ok(()), "member+established alpha is served");

    // Re-authenticate the SAME default context to `gamma` (api). This overwrites the
    // default context_role from `alpha` to `gamma`.
    auth(&mut host, "gamma", "api", "alice");

    // The default context now carries gamma's api credential. Probing `alpha`
    // (which does NOT accept `api`) must be HIDDEN as `unresolved` — identical to a
    // nonexistent role — because establishment now belongs to `gamma`, not `alpha`.
    let alpha = watch(&mut host, "alpha");
    let ghost = watch(&mut host, "ghost");
    assert_eq!(
        alpha,
        Err(DenialReason::Unresolved),
        "§10.4: after the swap `alpha` is unestablished on this context, so its \
         authenticator-not-accepted denial is hidden as `unresolved` — got {alpha:?}",
    );
    assert_eq!(alpha, ghost, "§10.4: swapped-away `alpha` and nonexistent `ghost` deny identically");

    // Establishment moved to `gamma`; alice is a member of gamma via api -> served.
    assert_eq!(watch(&mut host, "gamma"), Ok(()), "establishment moved to gamma; served");
}

/// DRY (probe b) — a caller that is a MEMBER of both `alpha` and `beta` through ONE
/// authentication context is SERVED on both. The default context authenticates to
/// `alpha`, but `beta` (which also accepts `token`, and of which alice is a member)
/// is served on the same context. There is no denial, so §10.4's established/hidden
/// distinction never engages — the per-named-role `established` flag being false for
/// `beta` does not wrongly deny a genuine member.
#[test]
fn one_context_member_of_both_roles_is_served_on_both() {
    let mut host = three_role_host();
    host.connect("c1").unwrap();

    auth(&mut host, "alpha", "token", "s_alice");

    assert_eq!(watch(&mut host, "alpha"), Ok(()), "member of alpha via default context is served");
    assert_eq!(
        watch(&mut host, "beta"),
        Ok(()),
        "§11.8: a member of beta through the SAME one token context is served — \
         establishment scoping does not deny a genuine member",
    );
}
