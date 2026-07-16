//! Shared fixtures for the surface integration tests (`MemoryStore`-backed).
//!
//! Each test binary uses a different subset, so unused-per-binary items are
//! expected. The fixtures build a real [`Engine`] over an in-memory store and
//! wire a [`SurfaceRouter`] whose bindings are re-validated against the model's
//! exposed surfaces — the same path a production host follows.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeMap;

use liasse_ident::InstanceId;
use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Claims, Credential, Engine, Precision, Role, RowSource, SessionAuthenticator,
    SessionSource, SurfaceAddress, SurfaceBinding, SurfaceCall, SurfaceHost, SurfaceOutcome,
    SurfaceRouter, SurfaceRouterBuilder, Timestamp, Value, Verifier, VerifyFailure, ViewBinding,
    VirtualClock,
};
use liasse_value::Text;

/// The fixed micro-precision "now" the tests run at — well before the seeded
/// sessions' `expires_at`.
pub const NOW: i128 = 1_700_000_000_000_000;

/// A far-future expiry (micros) shared by the live seeded sessions.
pub const FUTURE: i128 = 2_000_000_000_000_000;

/// A text value.
#[must_use]
pub fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// A micro-precision timestamp value.
#[must_use]
pub fn timestamp(count: i128) -> Value {
    Value::Timestamp(Timestamp::new(count, Precision::Micros))
}

/// An argument map from name/value pairs.
#[must_use]
pub fn args<const N: usize>(pairs: [(&str, Value); N]) -> BTreeMap<String, Value> {
    pairs.into_iter().map(|(name, value)| (name.to_owned(), value)).collect()
}

/// Parse a dotted surface address (test failure on a malformed one).
#[must_use]
pub fn address(text: &str) -> SurfaceAddress {
    SurfaceAddress::parse(text).expect("address parses")
}

/// A call to `target` with `pairs` as its arguments.
#[must_use]
pub fn call<const N: usize>(target: &str, pairs: [(&str, Value); N]) -> SurfaceCall {
    SurfaceCall::new(address(target), args(pairs))
}

/// A test verifier standing in for the §11.3 `$verify` namespace. It binds the
/// proof to `auth` and echoes the credential text as either the session key
/// (`session`) or the account key, so a session authenticator resolves a session
/// row and a stateless one resolves an account directly. A non-text credential
/// fails verification (a forged/malformed token).
pub struct TokenVerifier {
    auth: String,
    session: bool,
}

impl TokenVerifier {
    #[must_use]
    pub fn new(auth: &str, session: bool) -> Self {
        Self { auth: auth.to_owned(), session }
    }
}

impl Verifier for TokenVerifier {
    fn verify(&self, credential: &Credential) -> Result<Claims, VerifyFailure> {
        let Value::Text(_) = credential.value() else {
            return Err(VerifyFailure::new("credential is not a token"));
        };
        let key = credential.value().clone();
        if self.session {
            Ok(Claims::new(&self.auth, Some(key), None))
        } else {
            Ok(Claims::new(&self.auth, None, Some(key)))
        }
    }
}

/// The surface test application: accounts, sessions, tasks, the views the surface
/// layer resolves through, root mutations, a `token` authenticator, public
/// task/login surfaces, and a `member` role. Two accounts (`alice` enabled,
/// `bob` disabled) and three sessions (`s_alice` live, `s_bob` live but
/// disabled-account, `s_expired` past expiry) are seeded.
pub const SURFACE_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.surface@1.0.0"
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
      "remove": ".tasks - @id"
      "open_login": ".sessions + { id: @id, account: @account, expires_at: @expires }"
      "disable": ".accounts[@id].enabled = false"
      "revoke": ".sessions[@id].revoked = true"
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
      "tasks": {
        "$view": ".index"
        "$mut": { "add": ".add", "rename": ".rename", "remove": ".remove" }
      }
      "login": { "$mut": { "open": ".open_login" } }
      "session": { "$mut": { "revoke": ".revoke" } }
      "intake": { "$mut": { "add": ".add" } }
    }
    "$roles": {
      "member": {
        "$auth": "token"
        "$members": ".members_view"
        "tasks": {
          "$view": ".index"
          "$mut": { "complete": ".rename" }
        }
      }
    }
  }
  "$data": {
    "accounts": {
      "alice": { }
      "bob": { "enabled": false }
      "carol": { }
    }
    "sessions": {
      "s_alice": { "account": "alice", "expires_at": 2000000000000000 }
      "s_bob": { "account": "bob", "expires_at": 2000000000000000 }
      "s_carol": { "account": "carol", "expires_at": 2000000000000000 }
      "s_expired": { "account": "alice", "expires_at": 1000 }
    }
  }
}"#;

/// A fresh in-memory store for `instance`.
#[must_use]
pub fn store(instance: &str) -> MemoryStore {
    MemoryStore::new(InstanceId::new(instance))
}

/// Load [`SURFACE_APP`] and wire its router, returning a ready host at [`NOW`].
#[must_use]
pub fn host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = match Engine::load(store("surface"), SURFACE_APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    };
    let router = router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

/// Load [`SURFACE_APP`] into a fresh engine (for router-validation tests that
/// build their own routers against the model).
#[must_use]
pub fn loaded_engine() -> Engine<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    match Engine::load(store("surface"), SURFACE_APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    }
}

/// Build the router for [`SURFACE_APP`], validated against `model`.
fn router(model: &liasse_model::Model) -> SurfaceRouter {
    let public_tasks = SurfaceBinding::new()
        .with_view(ViewBinding::new("index"))
        .with_call("add", CallBinding::root("add", ["title".to_owned()]))
        .with_call("rename", CallBinding::root("rename", ["id".to_owned(), "title".to_owned()]))
        .with_call("remove", CallBinding::root("remove", ["id".to_owned()]));
    let login = SurfaceBinding::new().with_call(
        "open",
        CallBinding::root("open_login", ["id".to_owned(), "account".to_owned(), "expires".to_owned()]),
    );
    let session = SurfaceBinding::new()
        .with_call("revoke", CallBinding::root("revoke", ["id".to_owned()]));
    let intake = SurfaceBinding::new()
        .with_call("add", CallBinding::root("add", ["title".to_owned()]));
    let member_tasks = SurfaceBinding::new()
        .with_view(ViewBinding::new("index"))
        .with_call("complete", CallBinding::root("rename", ["id".to_owned(), "title".to_owned()]));

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
    let member = Role::new("member", ["token".to_owned()], RowSource::new("members_view", "id"));

    SurfaceRouterBuilder::new()
        .public_surface("tasks", public_tasks)
        .public_surface("login", login)
        .public_surface("session", session)
        .public_surface("intake", intake)
        .authenticator(Box::new(token))
        .authenticator(Box::new(api))
        .role(member, [("tasks".to_owned(), member_tasks)])
        .build(model)
        .expect("router validates against the model")
}

/// Add a task through the public surface and return its generated id (looked up
/// by its unique `title` in the `index` view).
pub fn add_task(host: &mut SurfaceHost<MemoryStore>, conn: &str, title: &str) -> Value {
    let outcome = host.call(conn, &call("public.tasks.add", [("title", text(title))])).expect("add");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "add commits: {outcome:?}");
    let view = host.engine().view_at_head("index").expect("view").expect("declared");
    let row = view.rows().iter().find(|row| row.field("title") == Some(&text(title))).expect("row present");
    row.field("id").cloned().expect("id")
}

/// Authenticate the `member` role's default context on `conn` with the token
/// `credential`, returning the result.
#[must_use]
pub fn authenticate_member(
    host: &mut SurfaceHost<MemoryStore>,
    conn: &str,
    credential: &str,
) -> liasse_surface::AuthResult {
    let request = liasse_surface::Authenticate::new(
        "member",
        liasse_surface::AuthSelection::new("token", Credential::new(text(credential))),
    );
    host.authenticate(conn, &request).expect("authenticate")
}
