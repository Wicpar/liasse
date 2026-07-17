#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §11.7 session validity across the four lifetime cases, driven end-to-end
//! through a real engine, router, and session authenticator.
//!
//! The session collection declares `expires_at: "timestamp? = none"`, so a row
//! may carry a finite expiry or none. §14 makes an omitted upper bound leave the
//! bucket interval unbounded, so §11.7 treats a session with no expiry as
//! perpetual — active until revoked — rather than denying it. This isolates that
//! rule: a perpetual session and a future-expiry session both authenticate,
//! while a past-expiry and a revoked session both deny (`DenialReason::SessionInvalid`).

mod support;

use liasse_ident::InstanceId;
use liasse_store::MemoryStore;
use liasse_surface::{
    AuthResult, AuthSelection, Authenticate, Credential, DenialReason, Engine, Precision, Role,
    RowSource, SessionAuthenticator, SessionSource, SurfaceBinding, SurfaceHost, SurfaceRouter,
    SurfaceRouterBuilder, ViewBinding, VirtualClock,
};
use support::{text, TokenVerifier, NOW};

/// A minimal app whose sessions carry an *optional* expiry. `s_perpetual` omits
/// it (defaulting to `none`); the others pin a finite expiry, one revoked. Both
/// finite instants sit far after [`NOW`], so only expiry sign, not clock drift,
/// decides each case.
const APP: &str = r#"{
  "$liasse": 1
  "$app": "example.perpetual@1.0.0"
  "$model": {
    "accounts": { "$key": "id", "id": "text", "enabled": "bool = true" }
    "sessions": {
      "$key": "id"
      "id": "text"
      "account": "text"
      "expires_at": "timestamp? = none"
      "revoked": "bool = false"
    }
    "sessions_view": { "$view": ".sessions { id, account, expires_at, revoked }" }
    "accounts_view": { "$view": ".accounts { id, enabled }" }
    "members_view": { "$view": ".accounts[:a | a.enabled] { id }" }
    "$auth": {
      "token": {
        "$credential": "text"
        "$verify": "token.verify($credential)"
        "$session": "/sessions[$proof.session]"
        "$actor": "/accounts[$session.account]"
        "$check": "$proof.auth == $auth_name"
      }
    }
    "$roles": {
      "member": {
        "$auth": "token"
        "$members": ".members_view"
        "accounts": { "$view": ".accounts_view" }
      }
    }
  }
  "$data": {
    "accounts": { "alice": { } }
    "sessions": {
      "s_perpetual": { "account": "alice" }
      "s_future": { "account": "alice", "expires_at": 2000000000000000 }
      "s_past": { "account": "alice", "expires_at": 1000 }
      "s_revoked": { "account": "alice", "expires_at": 2000000000000000, "revoked": true }
    }
  }
}"#;

fn host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = match Engine::load(MemoryStore::new(InstanceId::new("perpetual")), APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    };
    let router = router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn router(model: &liasse_model::Model) -> SurfaceRouter {
    let member_accounts = SurfaceBinding::new().with_view(ViewBinding::new("accounts_view"));
    let token = SessionAuthenticator::session(
        "token",
        Box::new(TokenVerifier::new("token", true)),
        SessionSource::new(RowSource::new("sessions_view", "id"), "account", "expires_at", "revoked"),
        RowSource::new("accounts_view", "id"),
    );
    let member = Role::new("member", ["token".to_owned()], RowSource::new("members_view", "id"));

    SurfaceRouterBuilder::new()
        .authenticator(Box::new(token))
        .role(member, [("accounts".to_owned(), member_accounts)])
        .build(model)
        .expect("router validates against the model")
}

fn authenticate(host: &mut SurfaceHost<MemoryStore>, credential: &str) -> AuthResult {
    let request =
        Authenticate::new("member", AuthSelection::new("token", Credential::new(text(credential))));
    host.authenticate("c1", &request).expect("authenticate")
}

fn denial_reason(result: AuthResult) -> DenialReason {
    match result {
        AuthResult::Denied(denial) => denial.reason(),
        AuthResult::Bound => panic!("expected a denial, got Bound"),
    }
}

#[test]
fn perpetual_session_authenticates() {
    // §11.7 + §14: a session with no expiry is unbounded above, so it stays valid
    // until revoked. It must bind, not deny.
    let mut host = host();
    host.connect("c1");
    assert!(
        matches!(authenticate(&mut host, "s_perpetual"), AuthResult::Bound),
        "a session with no expiry is perpetual and authenticates",
    );
}

#[test]
fn future_expiry_authenticates() {
    // A finite expiry strictly after `now` is still live (half-open interval).
    let mut host = host();
    host.connect("c1");
    assert!(
        matches!(authenticate(&mut host, "s_future"), AuthResult::Bound),
        "a session expiring in the future authenticates",
    );
}

#[test]
fn past_expiry_is_denied() {
    // At or after the expiry instant the session is dead (§11.7 half-open bound).
    let mut host = host();
    host.connect("c1");
    assert_eq!(denial_reason(authenticate(&mut host, "s_past")), DenialReason::SessionInvalid);
}

#[test]
fn revoked_session_is_denied() {
    // Revocation denies regardless of a still-future expiry (§11.7).
    let mut host = host();
    host.connect("c1");
    assert_eq!(denial_reason(authenticate(&mut host, "s_revoked")), DenialReason::SessionInvalid);
}
