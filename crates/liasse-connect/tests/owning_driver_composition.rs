#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Mutable composition accessors for an OWNING driver (Task 5-upstream scope
//! addition): a single-threaded driver that owns a [`ConnectCore`] by value must
//! still be able to (i) advance the surface clock per request
//! ([`ConnectCore::host_mut`] → [`SurfaceHost::advance_time`]) and (ii) admit an
//! INTERNAL, non-surface `$mut` the router cannot reach
//! ([`ConnectCore::host_mut`] → [`SurfaceHost::engine_mut`] → `Engine::call`),
//! *without* losing the authenticated surface protocol.
//!
//! This proves the two accessors compose: after advancing time and admitting an
//! internal mutation through the reclaimed `&mut Engine`, the surface path still
//! authenticates a role and serves its view.

use liasse_connect::{ConnectCore, Reply, Schema};
use liasse_ident::InstanceId;
use liasse_runtime::{CallOutcome, CallRequest};
use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Claims, Credential, Engine, Precision, Role, RowSource, SessionAuthenticator,
    SessionSource, SurfaceBinding, SurfaceHost, SurfaceRouter, SurfaceRouterBuilder, Verifier,
    VerifyFailure, ViewBinding, VirtualClock,
};
use liasse_value::{Text, Timestamp, Value};
use liasse_wire::serde_json::{json, Value as Json};
use liasse_wire::{ConnectionToken, Outcome, Sub, Upstream};

const NOW: i128 = 1_700_000_000_000_000;
/// One day in microseconds — the per-request clock advance the driver applies.
const P1D: i128 = 24 * 3600 * 1_000_000;

/// A package with a public `login` surface, a `member` role reading `roster`, and
/// an INTERNAL `note` mutation exposed on NO surface — the mutation an owning
/// driver must admit directly through the engine, never through the router.
const APP: &str = r#"{
  "$liasse": 1,
  "$app": "t.owndrv@1.0.0",
  "$requires": { "cose": "liasse.cose@1" },
  "$model": {
    "session_keys": { "$keyring": { "$provider": "test-kp", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" } },
    "accounts": { "$key": "id", "id": "text", "name": "text", "enabled": "bool = true" },
    "sessions": { "$key": "id", "id": "uuid = uuid()", "account": { "$ref": "/accounts" }, "revoked": "bool = false" },
    "logs": { "$key": "id", "id": "text", "note": "text" },
    "accounts_view": { "$view": ".accounts { id, enabled }" },
    "sessions_view": { "$view": ".sessions { id, account, revoked }" },
    "members_view": { "$view": ".accounts[:a | a.enabled] { id }" },
    "roster": { "$view": ".accounts { id, name, $sort: [id] }" },
    "logs_view": { "$view": ".logs { id, note, $sort: [id] }" },
    "$mut": {
      "login": [
        "session = /sessions + { account: @account }",
        "token = cose.sign(/session_keys, { auth: 'session', session: session.$key })",
        "return { token }"
      ],
      "note": [
        "entry = /logs + { id: @id, note: @note }",
        "return { entry: entry.$key }"
      ]
    },
    "$auth": {
      "session": {
        "$credential": "bytes",
        "$verify": "cose.verify(/session_keys, $credential)",
        "$session": "/sessions[$proof.session]",
        "$actor": "/accounts[$session.account]",
        "$check": ["$proof.auth == $auth_name", "!$session.revoked"]
      }
    },
    "$public": { "login": { "$mut": { "open": ".login" } } },
    "$roles": {
      "member": {
        "$auth": "session",
        "$members": ".members_view",
        "roster": { "$view": ".roster" }
      }
    }
  },
  "$data": { "accounts": { "alice": { "name": "Alice" } } }
}"#;

/// The surface `$verify` receives already-cose-verified claims (a `Value::Struct`)
/// or a non-struct sentinel; it only decodes the claims (mirrors `native_cose.rs`).
struct CoseVerifier;

impl Verifier for CoseVerifier {
    fn verify(&self, credential: &Credential) -> Result<Claims, VerifyFailure> {
        let Value::Struct(claims) = credential.value() else {
            return Err(VerifyFailure::new("cose token did not verify against the keyring"));
        };
        let Some(Value::Text(auth)) = claims.get("auth") else {
            return Err(VerifyFailure::new("verified cose claims carry no `auth` binding"));
        };
        Ok(Claims::new(auth.as_str(), claims.get("session").cloned(), claims.get("account").cloned()))
    }
}

fn router(model: &liasse_model::Model) -> SurfaceRouter {
    let login = SurfaceBinding::new().with_call("open", CallBinding::root("login", ["account".to_owned()]));
    let member_roster = SurfaceBinding::new().with_view(ViewBinding::new("roster"));
    let session = SessionAuthenticator::session(
        "session",
        Box::new(CoseVerifier),
        SessionSource::new(RowSource::new("sessions_view", "id"), "account", "expires_at", "revoked"),
        RowSource::new("accounts_view", "id"),
    );
    let member = Role::new("member", ["session".to_owned()], RowSource::new("members_view", "id"));
    SurfaceRouterBuilder::new()
        .public_surface("login", login)
        .authenticator(Box::new(session))
        .role(member, [("roster".to_owned(), member_roster)])
        .build(model)
        .expect("router validates against the model")
}

fn schema(engine: &Engine<MemoryStore>) -> Schema {
    Schema::builder()
        .call("public.login.open", [("account".to_owned(), liasse_value::Type::Text)])
        .view("member.roster", engine.surface_view_params("member.roster"))
        .cose("session", "session_keys")
        .build()
}

/// A sim-backed core (the ring self-provisions the sim double; keyring wiring is
/// not what this test exercises), mounted by value as an owning driver would hold it.
fn mount() -> ConnectCore<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let store = MemoryStore::new(InstanceId::new("owning-driver"));
    let engine = Engine::load(store, APP, &mut clock).expect("load");
    let schema = schema(&engine);
    let router = router(engine.model());
    let host = SurfaceHost::new(engine, router, clock);
    ConnectCore::mount(host, schema)
}

fn hello_anon(core: &mut ConnectCore<MemoryStore>) -> ConnectionToken {
    match core.submit(None, None, Upstream::Hello { auth: None, context: None }) {
        Ok(Reply::Hello { connection }) => connection,
        other => panic!("hello: {other:?}"),
    }
}

fn hello_member(core: &mut ConnectCore<MemoryStore>, credential: &Json) -> ConnectionToken {
    let auth = json!({ "role": "member", "auth": "session", "credential": credential });
    match core.submit(None, None, Upstream::Hello { auth: Some(auth), context: None }) {
        Ok(Reply::Hello { connection }) => connection,
        other => panic!("hello_member: {other:?}"),
    }
}

fn login_token(core: &mut ConnectCore<MemoryStore>, conn: &ConnectionToken) -> Json {
    let frame = Upstream::Call {
        address: "public.login.open".to_owned(),
        args: json!({ "account": "alice" }),
        auth: None,
        context: None,
    };
    match core.submit(Some(conn), None, frame) {
        Ok(Reply::Outcome(Outcome::Committed { response: Some(value), .. })) => {
            value.get("token").cloned().expect("the login returns a `token`")
        }
        other => panic!("login: {other:?}"),
    }
}

fn view(core: &mut ConnectCore<MemoryStore>, conn: &ConnectionToken, address: &str) -> Reply {
    let frame = Upstream::View {
        sub: Sub::new("s1"),
        address: address.to_owned(),
        params: None,
        window: None,
        auth: None,
        context: None,
    };
    core.submit(Some(conn), None, frame).expect("view reply")
}

/// The owning driver advances the surface clock and admits an internal, non-surface
/// mutation through the reclaimed `&mut Engine`, then the surface path still
/// authenticates a role and serves its view.
#[test]
fn owning_driver_advances_time_and_admits_internal_mut_then_surface_authenticates() {
    let mut core = mount();

    // (i) Per-request clock advance through the reclaimed `&mut SurfaceHost` — the
    // set_time-per-request discipline an owning wall-clock driver needs.
    let advanced = Timestamp::new(NOW + P1D, Precision::Micros);
    core.host_mut().advance_time(advanced).expect("advance_time through host_mut");
    assert_eq!(core.host().engine().now(), advanced, "the engine clock advanced");

    // (ii) Admit an INTERNAL mutation the router exposes on no surface, directly
    // through host_mut().engine_mut() — unreachable via any surface `call`.
    let mut gens = VirtualClock::new(NOW + P1D, Precision::Micros);
    let request = CallRequest::new("note")
        .arg("id", Value::Text(Text::new("l1")))
        .arg("note", Value::Text(Text::new("internal audit entry")));
    let outcome = core.host_mut().engine_mut().call(&request, &mut gens).expect("internal admission");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the internal non-surface mut committed");

    // (iii) The surface protocol still authenticates a role and serves its view.
    let anon = hello_anon(&mut core);
    let token = login_token(&mut core, &anon);
    let member = hello_member(&mut core, &token);
    assert!(
        matches!(view(&mut core, &member, "member.roster"), Reply::Opened { .. }),
        "the authenticated member reads the role view after the internal composition",
    );
    assert!(
        matches!(view(&mut core, &anon, "member.roster"), Reply::Outcome(Outcome::Denied { .. })),
        "an unauthenticated connection is still denied the role view",
    );
}
