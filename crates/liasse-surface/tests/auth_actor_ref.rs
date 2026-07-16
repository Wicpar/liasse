#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §11.3 end-to-end with a ref-typed session `account`: the spec's own §11.2
//! session shape declares `account: { $ref: "/accounts" }`, so `$session.account`
//! projects as a `Value::Ref`. Resolving `$actor` must dereference that ref to the
//! accounts collection's scalar key (§5.6), and the admitted mutation must run
//! with `$actor` bound to the resolved account (§11.1). This exercises both the
//! `RowSource` ref-match fix and the surface→runtime actor threading together —
//! the flagship `11-auth/authenticated-call-resolves-actor` shape.

mod support;

use liasse_ident::InstanceId;
use liasse_store::MemoryStore;
use liasse_surface::{
    Authenticate, AuthResult, AuthSelection, CallBinding, Credential, Engine, Precision, Role,
    RowSource, SessionAuthenticator, SessionSource, SurfaceBinding, SurfaceCall, SurfaceHost,
    SurfaceOutcome, SurfaceRouter, SurfaceRouterBuilder, ViewBinding, VirtualClock,
};
use support::{address, args, text, timestamp, TokenVerifier, FUTURE, NOW};

/// Like [`support::SURFACE_APP`] but the session's `account` is a ref (§11.2's
/// declared shape), and a `member` role exposes a mutation that writes `$actor`.
const REF_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.refauth@1.0.0"
  "$model": {
    "accounts": { "$key": "id", "id": "text", "enabled": "bool = true" }
    "sessions": {
      "$key": "id"
      "id": "text"
      "account": { "$ref": "/accounts" }
      "expires_at": "timestamp"
      "revoked": "bool = false"
    }
    "notes": {
      "$key": "id"
      "id": "text"
      "author": { "$ref": "/accounts" }
      "body": "text"
    }
    "sessions_view": { "$view": ".sessions { id, account, expires_at, revoked }" }
    "accounts_view": { "$view": ".accounts { id, enabled }" }
    "members_view": { "$view": ".accounts[:a | a.enabled] { id }" }
    "notes_view": { "$view": ".notes { id, author, body }" }
    "$mut": {
      "add_note({ id: text, body: text })": [
        "note = .notes + { id: @id, author: $actor, body: @body }"
        "return note { id, author, body }"
      ]
      "open_login": ".sessions + { id: @id, account: @account, expires_at: @expires }"
    }
    "$auth": {
      "token": {
        "$credential": "text"
        "$verify": "token.verify($credential)"
        "$session": "/sessions[$proof.session]"
        "$actor": "/accounts[$session.account]"
        "$check": "$proof.auth == $auth_name"
      }
    }
    "$public": {
      "login": { "$mut": { "open": ".open_login" } }
    }
    "$roles": {
      "member": {
        "$auth": "token"
        "$members": ".members_view"
        "notes": { "$view": ".notes_view", "$mut": { "add": ".add_note" } }
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

fn host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = match Engine::load(MemoryStore::new(InstanceId::new("refauth")), REF_APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    };
    let router = router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn router(model: &liasse_model::Model) -> SurfaceRouter {
    let login = SurfaceBinding::new().with_call(
        "open",
        CallBinding::root("open_login", ["id".to_owned(), "account".to_owned(), "expires".to_owned()]),
    );
    let member_notes = SurfaceBinding::new()
        .with_view(ViewBinding::new("notes_view"))
        .with_call("add", CallBinding::root("add_note", ["id".to_owned(), "body".to_owned()]));

    let token = SessionAuthenticator::session(
        "token",
        Box::new(TokenVerifier::new("token", true)),
        SessionSource::new(RowSource::new("sessions_view", "id"), "account", "expires_at", "revoked"),
        RowSource::new("accounts_view", "id"),
    );
    let member = Role::new("member", ["token".to_owned()], RowSource::new("members_view", "id"));

    SurfaceRouterBuilder::new()
        .public_surface("login", login)
        .authenticator(Box::new(token))
        .role(member, [("notes".to_owned(), member_notes)])
        .build(model)
        .expect("router validates against the model")
}

fn authenticate(host: &mut SurfaceHost<MemoryStore>, conn: &str, credential: &str) -> AuthResult {
    let request =
        Authenticate::new("member", AuthSelection::new("token", Credential::new(text(credential))));
    host.authenticate(conn, &request).expect("authenticate")
}

#[test]
fn ref_typed_session_account_resolves_the_actor() {
    // §5.6: `$session.account` is a `Value::Ref`; resolving `$actor` must deref it
    // to the accounts key. Before the fix this denied with `ActorUnresolved`.
    let mut host = host();
    host.connect("c1");
    assert!(
        matches!(authenticate(&mut host, "c1", "s_alice"), AuthResult::Bound),
        "a ref-typed session account resolves the actor account row"
    );
}

#[test]
fn authenticated_member_call_binds_actor_and_commits() {
    // The full flagship shape: authenticate, then a role mutation writing `$actor`
    // into a ref field commits, and its `return` projects the actor's account key.
    let mut host = host();
    host.connect("c1");
    assert!(matches!(authenticate(&mut host, "c1", "s_alice"), AuthResult::Bound));

    let outcome = host
        .call("c1", &SurfaceCall::new(address("member.notes.add"), args([("id", text("n1")), ("body", text("hello"))])))
        .expect("call");
    let SurfaceOutcome::Committed { response, .. } = outcome else {
        panic!("authenticated member call should commit, got {outcome:?}");
    };
    let wire = response.expect("a return value").to_wire();
    assert_eq!(wire.get("author").and_then(|v| v.as_str()), Some("alice"), "author is the actor key");
    assert_eq!(wire.get("body").and_then(|v| v.as_str()), Some("hello"));
}

#[test]
fn login_then_actor_backed_call_round_trips() {
    // §11.5: a login inserts a fresh (ref-account) session, and the freshly-issued
    // session immediately authenticates and admits an `$actor`-reading mutation.
    let mut host = host();
    host.connect("c1");
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

    assert!(matches!(authenticate(&mut host, "c1", "s_new"), AuthResult::Bound), "new session authenticates");
    let outcome = host
        .call("c1", &SurfaceCall::new(address("member.notes.add"), args([("id", text("n2")), ("body", text("hi"))])))
        .expect("call");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "freshly-issued session admits an actor-backed call");
}
