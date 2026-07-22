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
    CallBinding, Claims, Credential, Engine, Entropy, PatchOp, Precision, Role, RowId, RowSource,
    SessionAuthenticator, SessionSource, SurfaceAddress, SurfaceBinding, SurfaceCall, SurfaceHost,
    SurfaceOutcome, SurfaceRouter, SurfaceRouterBuilder, Timestamp, Value, Verifier, VerifyFailure,
    ViewBinding, ViewDelta, ViewRow, VirtualClock,
};
use liasse_value::Text;
use liasse_wire::{Occ, WireRow};

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
    // These fixtures assert on generated `uuid()` values by lookup and by
    // capture-and-reuse within a single run, so admission must be reproducible.
    // Production defaults to the OS CSPRNG (§5.1/§8.12); a deterministic seeded
    // source keeps the fixtures replayable through the same injection seam a real
    // deployment would use for the clock.
    SurfaceHost::new(engine, router, clock).with_entropy(Entropy::seeded(0x5EED))
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

// --- the ONE §12.2 client-side patch applier, shared by every red_* test --------
//
// The runtime's `patch::diff` produces a `ViewDelta`; applying it existed only as
// five duplicated copies inside these tests. This adapter is the single applier
// they now share: it projects the runtime's `ViewRow`/`RowId` onto the engine-free
// wire types, delegates to `liasse_wire::apply` (the one source of truth for §12.2
// apply semantics), and maps the result back. The tests' own `visible(..)`
// assertions are unchanged, so the fact that they still pass proves the wire applier
// matches what each local copy used to compute.

/// A per-call, injective `RowId` -> occurrence-token map. The occurrence identity a
/// client sees is opaque (SPEC.md §12.2), so the wire never carries the `RowId`; a
/// dense counter keyed by `RowId` is a faithful stand-in for the bijection the
/// server would mint per subscription.
#[derive(Default)]
struct OccMap {
    next: u64,
    ids: BTreeMap<RowId, String>,
}

impl OccMap {
    fn of(&mut self, id: &RowId) -> String {
        if let Some(token) = self.ids.get(id) {
            return token.clone();
        }
        let token = format!("occ-{}", self.next);
        self.next += 1;
        self.ids.insert(id.clone(), token.clone());
        token
    }
}

/// A faithful §12.2 client that applies `delta` to `prior` by delegating to the
/// shared `liasse_wire::apply`. The result carries the same occurrences, values, and
/// order the local copies produced, so callers assert on it exactly as before.
#[must_use]
pub fn apply_patch(prior: &[ViewRow], delta: &ViewDelta) -> Vec<ViewRow> {
    let ops = match delta {
        ViewDelta::Init(rows) => return rows.clone(),
        ViewDelta::Scalar(_) => panic!("a row-stream view never yields a scalar delta"),
        ViewDelta::Patch(ops) => ops,
    };

    // `occ` is the RowId<->token bijection; `registry` remembers the latest `ViewRow`
    // behind each token so the applied wire rows can be mapped straight back to the
    // runtime rows the assertions compare (a `ViewRow` cannot be reconstructed from
    // the wire alone — its fields are private to the runtime).
    let mut occ = OccMap::default();
    let mut registry: BTreeMap<String, ViewRow> = BTreeMap::new();

    let prev: Vec<WireRow> = prior.iter().map(|row| wire_row(row, &mut occ, &mut registry)).collect();
    let wire_ops: Vec<liasse_wire::PatchOp> =
        ops.iter().map(|op| wire_op(op, &mut occ, &mut registry)).collect();

    let applied = liasse_wire::apply(&prev, &wire_ops)
        .expect("liasse_wire::apply reproduces the diff result without error");

    applied
        .iter()
        .map(|row| registry.get(row.occ().as_str()).cloned().expect("every applied occ was registered"))
        .collect()
}

/// Project one runtime row onto a wire row, recording it under its token. The wire
/// value is left as `null`: these tests assert on occurrence identity and order
/// (`visible(..)`), which the reconstruction recovers from the registry, so the
/// carried value is immaterial here — `apply`'s value handling is proven by
/// `liasse-wire`'s own unit tests.
fn wire_row(row: &ViewRow, occ: &mut OccMap, registry: &mut BTreeMap<String, ViewRow>) -> WireRow {
    let token = register(row, occ, registry);
    WireRow::new(Occ::new(token), liasse_wire::Value::Null)
}

/// Translate one runtime patch op to its wire form, registering any row it carries.
fn wire_op(op: &PatchOp, occ: &mut OccMap, registry: &mut BTreeMap<String, ViewRow>) -> liasse_wire::PatchOp {
    match op {
        PatchOp::Insert { at, row } => {
            let token = register(row, occ, registry);
            liasse_wire::PatchOp::Insert { at: *at, occ: Occ::new(token), value: liasse_wire::Value::Null }
        }
        PatchOp::Update { row } => {
            let token = register(row, occ, registry);
            liasse_wire::PatchOp::Update { occ: Occ::new(token), value: liasse_wire::Value::Null }
        }
        PatchOp::Remove { id } => liasse_wire::PatchOp::Remove { occ: Occ::new(occ.of(id)) },
        PatchOp::Move { id, to } => liasse_wire::PatchOp::Move { occ: Occ::new(occ.of(id)), to: *to },
        PatchOp::Rekey { .. } => panic!("diff renders a key change as remove+insert, never rekey"),
    }
}

/// Mint (or reuse) the token for `row`'s occurrence and record the latest row
/// behind it, so an `update` reconstructs to the new row and an `insert` to the new
/// occurrence.
fn register(row: &ViewRow, occ: &mut OccMap, registry: &mut BTreeMap<String, ViewRow>) -> String {
    let token = occ.of(row.id());
    registry.insert(token.clone(), row.clone());
    token
}
