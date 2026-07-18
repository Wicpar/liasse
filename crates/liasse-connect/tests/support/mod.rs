//! Shared fixtures for the loopback conformance suite.
//!
//! The suite drives a [`ConnectCore`] in process over the `SURFACE_APP` fixture (the
//! same app the surface integration tests use), with a [`liasse_wire::WireStore`]
//! standing in for the untrusted client: it consumes the decoded downstream frames
//! and folds them into the §12.2 replica, so a test asserts the client's *applied*
//! state equals the server's *recomputed* authorized view.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeMap;

use liasse_connect::{ConnectCore, Reply, Schema};
use liasse_ident::InstanceId;
use liasse_model::Model;
use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Credential, Engine, Precision, Role, RowSource, SessionAuthenticator, SessionSource,
    SurfaceAddress, SurfaceBinding, SurfaceHost, SurfaceRouter, SurfaceRouterBuilder, ViewBinding,
    VirtualClock,
};
use liasse_value::{Text, Type, Value};
use liasse_wire::serde_json::{Value as Json, json};
use liasse_wire::{
    ConnectionToken, Downstream, Ft, Occ, OperationId, Outcome, SseEvent, Sub, Upstream, WireStore,
    WireWindow,
};

// --- the surface test application (copied from liasse-surface's test support, whose
// fixtures are not a library export) ------------------------------------------------

pub const NOW: i128 = 1_700_000_000_000_000;

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

/// The §11.3 `$verify` stand-in: binds the proof to `auth` and echoes the credential
/// text as the session or account key.
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

impl liasse_surface::Verifier for TokenVerifier {
    fn verify(
        &self,
        credential: &Credential,
    ) -> Result<liasse_surface::Claims, liasse_surface::VerifyFailure> {
        let Value::Text(_) = credential.value() else {
            return Err(liasse_surface::VerifyFailure::new("credential is not a token"));
        };
        let key = credential.value().clone();
        if self.session {
            Ok(liasse_surface::Claims::new(&self.auth, Some(key), None))
        } else {
            Ok(liasse_surface::Claims::new(&self.auth, None, Some(key)))
        }
    }
}

fn router(model: &Model) -> SurfaceRouter {
    let public_tasks = SurfaceBinding::new()
        .with_view(ViewBinding::new("index"))
        .with_call("add", CallBinding::root("add", ["title".to_owned()]))
        .with_call("rename", CallBinding::root("rename", ["id".to_owned(), "title".to_owned()]))
        .with_call("remove", CallBinding::root("remove", ["id".to_owned()]));
    let login = SurfaceBinding::new().with_call(
        "open",
        CallBinding::root("open_login", ["id".to_owned(), "account".to_owned(), "expires".to_owned()]),
    );
    let session = SurfaceBinding::new().with_call("revoke", CallBinding::root("revoke", ["id".to_owned()]));
    let intake = SurfaceBinding::new().with_call("add", CallBinding::root("add", ["title".to_owned()]));
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

/// The decode contract derived from the model: each call address bound to its
/// mutation's typed parameter set, each view's `$params`, and the `$credential` types.
fn schema(engine: &Engine<MemoryStore>) -> Schema {
    let model = engine.model();
    Schema::builder()
        .call("public.tasks.add", mutation_args(model, "add"))
        .call("public.tasks.rename", mutation_args(model, "rename"))
        .call("public.tasks.remove", mutation_args(model, "remove"))
        .call("public.login.open", mutation_args(model, "open_login"))
        .call("public.session.revoke", mutation_args(model, "revoke"))
        .call("public.intake.add", mutation_args(model, "add"))
        .call("member.tasks.complete", mutation_args(model, "rename"))
        .view("public.tasks", engine.surface_view_params("public.tasks"))
        .view("member.tasks", engine.surface_view_params("member.tasks"))
        .credential("token", Type::Text)
        .credential("api", Type::Text)
        .build()
}

/// The typed argument contract of a root mutation, from the model's inferred params.
fn mutation_args(model: &Model, name: &str) -> Vec<(String, Type)> {
    model
        .mutations()
        .iter()
        .find(|m| m.path.is_empty() && m.name.as_str() == name)
        .map(|m| {
            m.params
                .iter()
                .filter_map(|(param, ty)| ty.as_scalar().map(|scalar| (param.clone(), scalar.clone())))
                .collect()
        })
        .unwrap_or_default()
}

/// A mounted core over the surface app, ready at [`NOW`].
#[must_use]
pub fn app() -> ConnectCore<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = match Engine::load(MemoryStore::new(InstanceId::new("connect")), SURFACE_APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    };
    let schema = schema(&engine);
    let router = router(engine.model());
    let host = SurfaceHost::new(engine, router, clock);
    ConnectCore::mount(host, schema)
}

// --- driving helpers ---------------------------------------------------------------

/// A text value.
#[must_use]
pub fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// Open a connection with no authentication.
#[must_use]
pub fn hello(core: &mut ConnectCore<MemoryStore>) -> ConnectionToken {
    match core.submit(None, None, Upstream::Hello { auth: None, context: None }) {
        Ok(Reply::Hello { connection }) => connection,
        other => panic!("hello: {other:?}"),
    }
}

/// Open a connection authenticated as the `member` role with `credential`.
#[must_use]
pub fn hello_member(core: &mut ConnectCore<MemoryStore>, credential: &str) -> ConnectionToken {
    let auth = json!({ "role": "member", "auth": "token", "credential": credential });
    match core.submit(None, None, Upstream::Hello { auth: Some(auth), context: None }) {
        Ok(Reply::Hello { connection }) => connection,
        other => panic!("hello_member: {other:?}"),
    }
}

/// Open a subscription over `address` (unwindowed) and return its opening frontier.
pub fn view(core: &mut ConnectCore<MemoryStore>, conn: &ConnectionToken, sub: &str, address: &str) -> Ft {
    view_frame(core, conn, view_request(sub, address, None))
}

/// A `view` upstream frame.
#[must_use]
pub fn view_request(sub: &str, address: &str, window: Option<WireWindow>) -> Upstream {
    Upstream::View {
        sub: Sub::new(sub),
        address: address.to_owned(),
        params: None,
        window,
        auth: None,
        context: None,
    }
}

/// Submit a `view` frame, expecting it to open, and return its frontier.
pub fn view_frame(core: &mut ConnectCore<MemoryStore>, conn: &ConnectionToken, frame: Upstream) -> Ft {
    match core.submit(Some(conn), None, frame) {
        Ok(Reply::Opened { frontier }) => frontier,
        other => panic!("view: {other:?}"),
    }
}

/// Submit a `view` frame and return the whole reply (for refusal assertions).
pub fn view_reply(core: &mut ConnectCore<MemoryStore>, conn: &ConnectionToken, frame: Upstream) -> Reply {
    core.submit(Some(conn), None, frame).expect("view reply")
}

/// Invoke a call with JSON `args`, optionally carrying an operation id.
pub fn call(
    core: &mut ConnectCore<MemoryStore>,
    conn: &ConnectionToken,
    address: &str,
    args: Json,
    operation: Option<&str>,
) -> Outcome {
    let frame = Upstream::Call { address: address.to_owned(), args, auth: None, context: None };
    let op = operation.map(OperationId::new);
    match core.submit(Some(conn), op, frame) {
        Ok(Reply::Outcome(outcome)) => outcome,
        other => panic!("call: {other:?}"),
    }
}

/// The current SSE events buffered for `conn` (the live-writer drain).
pub fn drain(core: &mut ConnectCore<MemoryStore>, conn: &ConnectionToken) -> Vec<SseEvent> {
    core.poll(conn).expect("poll")
}

/// The generated uuid of the task titled `title`, as a wire value (for a call arg).
pub fn task_id_json(core: &ConnectCore<MemoryStore>, title: &str) -> Json {
    let view = core.host().engine().view_at_head("index").expect("view").expect("declared");
    view.rows()
        .iter()
        .find(|row| row.field("title") == Some(&text(title)))
        .and_then(|row| row.field("id"))
        .map(Value::to_wire)
        .expect("task id")
}

/// A §12.2 client replica: one [`WireStore`] per subscription, fed decoded downstream
/// frames exactly as the untrusted web client would apply them.
#[derive(Default)]
pub struct Client {
    subs: BTreeMap<Sub, WireStore>,
}

impl Client {
    /// A fresh client with no subscriptions.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold a batch of SSE events into the per-subscription replicas.
    pub fn feed(&mut self, events: &[SseEvent]) {
        for event in events {
            let ft = Ft::new(event.id.clone().unwrap_or_default());
            let frame: Downstream = liasse_wire::decode(&event.data).expect("decode downstream");
            self.apply(frame, ft);
        }
    }

    fn apply(&mut self, frame: Downstream, ft: Ft) {
        match frame {
            Downstream::Init { sub, rows } => {
                self.subs.entry(sub).or_default().init(rows, ft).expect("init");
            }
            Downstream::Scalar { sub, value } => {
                self.subs.entry(sub).or_default().scalar(value, ft).expect("scalar");
            }
            Downstream::Patch { sub, ops } => {
                self.subs.get_mut(&sub).expect("patch targets a live sub").patch(&ops, ft).expect("patch");
            }
            Downstream::Close { sub, reason } => {
                self.subs.entry(sub).or_default().close(reason);
            }
            Downstream::Frontier => {
                for store in self.subs.values_mut() {
                    let _ = store.advance_frontier(ft.clone());
                }
            }
            // A reset drops the client's state; the fresh init that follows in the
            // same batch re-creates each subscription from scratch.
            Downstream::Reset { .. } => self.subs.clear(),
            Downstream::Fault { .. } => {}
        }
    }

    /// The rows a subscription's replica currently holds.
    #[must_use]
    pub fn rows(&self, sub: &str) -> Vec<Json> {
        self.subs
            .get(&Sub::new(sub))
            .map(|store| store.rows().iter().map(|row| row.value().clone()).collect())
            .unwrap_or_default()
    }

    /// The `title` field of each row a subscription holds, in order.
    #[must_use]
    pub fn titles(&self, sub: &str) -> Vec<String> {
        self.rows(sub)
            .iter()
            .filter_map(|row| row.get("title").and_then(Json::as_str).map(str::to_owned))
            .collect()
    }

    /// The occurrence tokens a subscription holds, in order.
    #[must_use]
    pub fn occ(&self, sub: &str) -> Vec<Occ> {
        self.subs
            .get(&Sub::new(sub))
            .map(|store| store.rows().iter().map(|row| row.occ().clone()).collect())
            .unwrap_or_default()
    }

    /// The scalar value a subscription's replica holds, if it is a scalar view.
    #[must_use]
    pub fn scalar(&self, sub: &str) -> Option<Json> {
        self.subs.get(&Sub::new(sub)).and_then(|store| store.scalar_value().cloned())
    }

    /// Whether a subscription has been closed.
    #[must_use]
    pub fn closed(&self, sub: &str) -> bool {
        self.subs.get(&Sub::new(sub)).is_some_and(|store| store.close_reason().is_some())
    }
}

/// The `title` order of a surface view read directly from the host — the recomputed
/// authorized state a client's applied replica must equal.
#[must_use]
pub fn server_titles(core: &ConnectCore<MemoryStore>, conn: &ConnectionToken, sub: &str) -> Vec<String> {
    let Some(view) = core.host().read_view(conn.as_str(), sub) else {
        return Vec::new();
    };
    view.rows()
        .iter()
        .filter_map(|row| match row.field("title") {
            Some(Value::Text(text)) => Some(text.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

/// Parse a surface address (test failure on a malformed one).
#[must_use]
pub fn address(text: &str) -> SurfaceAddress {
    SurfaceAddress::parse(text).expect("address parses")
}
