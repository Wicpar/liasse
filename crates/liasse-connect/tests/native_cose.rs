#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §17.7/§17.8 native-cose authentication end-to-end through [`ConnectCore`], over
//! an engine whose `session_keys` keyring is backed by an INJECTED software
//! [`Ed25519KeyProvider`] (§17.5, Task 5-upstream).
//!
//! The happy path: an anonymous connection mints a cose token on a public login
//! surface (`cose.sign` over the injected ring); a second connection authenticates
//! the `member` role by presenting that token as its credential; the connector
//! gates the raw wire token through the engine's `cose.verify`
//! ([`ConnectCore::decode_selection`]) so the surface verifier only ever sees the
//! VERIFIED CLAIMS; and the authenticated connection reads a role-only view. A
//! tampered/forged token, and a token minted by a SIM-backed engine, are each
//! DENIED — the injected Ed25519 key is not the reconstructable sim key, so a
//! forgeable token cannot authenticate. `ConnectCore` is also exercised over a
//! non-default provider param (`SurfaceHost<_, Ed25519KeyProvider>`), proving the
//! genericization.

use liasse_connect::{ConnectCore, Reply, Schema};
use liasse_ident::InstanceId;
use liasse_key_ed25519::Ed25519KeyProvider;
use liasse_runtime::Registry;
use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Claims, Credential, Engine, KeyProvider, Precision, Role, RowSource,
    SessionAuthenticator, SessionSource, SurfaceBinding, SurfaceHost, SurfaceRouter,
    SurfaceRouterBuilder, Verifier, VerifyFailure, ViewBinding, VirtualClock,
};
use liasse_value::{Type, Value};
use liasse_wire::serde_json::{json, Value as Json};
use liasse_wire::{ConnectionToken, Outcome, Sub, Upstream};

const NOW: i128 = 1_700_000_000_000_000;

/// A keyring package minting native-cose session tokens: a public `login` that
/// signs a token through the `session_keys` ring, a `session` authenticator whose
/// `$verify` is `cose.verify(/session_keys, …)`, and a `member` role with a
/// role-only `roster` view. The synthetic actor/session/member views the surface
/// row-sources read are declared explicitly.
const COSE_APP: &str = r#"{
  "$liasse": 1,
  "$app": "t.cose@1.0.0",
  "$requires": { "cose": "liasse.cose@1" },
  "$model": {
    "session_keys": { "$keyring": { "$provider": "test-kp", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" } },
    "accounts": { "$key": "id", "id": "text", "name": "text", "enabled": "bool = true" },
    "sessions": { "$key": "id", "id": "uuid = uuid()", "account": { "$ref": "/accounts" }, "revoked": "bool = false" },
    "accounts_view": { "$view": ".accounts { id, enabled }" },
    "sessions_view": { "$view": ".sessions { id, account, revoked }" },
    "members_view": { "$view": ".accounts[:a | a.enabled] { id }" },
    "roster": { "$view": ".accounts { id, name, $sort: [id] }" },
    "$mut": {
      "login": [
        "session = /sessions + { account: @account }",
        "token = cose.sign(/session_keys, { auth: 'session', session: session.$key })",
        "return { token }"
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

/// The surface `$verify: "cose.verify(/ring, …)"` verifier (§17.7): the token is
/// verified against the ring at the connector's auth layer *before* this runs, so
/// the credential it receives is the already-verified claims struct — or a
/// non-struct sentinel a denied token was replaced by. It only decodes the claims.
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

/// The surface router: a public `login.open` call, the cose `session`
/// authenticator, and a `member` role exposing the `roster` view.
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

/// The typed decode contract: the login call args, the role view params, and the
/// `session` authenticator's keyring (so the connector gates its cose credential).
fn schema(engine: &Engine<MemoryStore>) -> Schema {
    Schema::builder()
        .call("public.login.open", [("account".to_owned(), Type::Text)])
        .view("member.roster", engine.surface_view_params("member.roster"))
        .cose("session", "session_keys")
        .build()
}

/// Build a mounted core over `COSE_APP`. When `registry` is supplied it backs the
/// `session_keys` ring with the injected Ed25519 provider; otherwise the ring
/// self-provisions its sim double. `P` is the surface's (unused, driver-facing)
/// provider param — set to `Ed25519KeyProvider` on the injected core to exercise
/// `ConnectCore`'s genericization over a non-default provider.
fn mount<P: KeyProvider>(instance: &str, registry: Option<Registry>) -> ConnectCore<MemoryStore, P> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let store = MemoryStore::new(InstanceId::new(instance));
    let engine = match registry {
        Some(registry) => Engine::load_with_hosts(store, COSE_APP, &mut clock, registry).expect("injected load"),
        None => Engine::load(store, COSE_APP, &mut clock).expect("sim load"),
    };
    let schema = schema(&engine);
    let router = router(engine.model());
    let host: SurfaceHost<MemoryStore, P> = SurfaceHost::new(engine, router, clock);
    ConnectCore::mount(host, schema)
}

/// A registry backing `test-kp` with the real software Ed25519 provider (§17.5).
fn ed25519_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register_provider("test-kp", Box::new(Ed25519KeyProvider::new()) as Box<dyn KeyProvider>);
    registry
}

/// An injected-Ed25519 core, exercised over a non-default surface provider param.
fn injected_core() -> ConnectCore<MemoryStore, Ed25519KeyProvider> {
    mount("cose-injected", Some(ed25519_registry()))
}

fn hello_anon<P: KeyProvider>(core: &mut ConnectCore<MemoryStore, P>) -> ConnectionToken {
    match core.submit(None, None, Upstream::Hello { auth: None, context: None }) {
        Ok(Reply::Hello { connection }) => connection,
        other => panic!("hello: {other:?}"),
    }
}

/// Authenticate the `member` role by presenting `credential` (a cose token wire
/// object) for the `session` authenticator, returning the (bound-or-not) connection.
fn hello_member<P: KeyProvider>(core: &mut ConnectCore<MemoryStore, P>, credential: &Json) -> ConnectionToken {
    let auth = json!({ "role": "member", "auth": "session", "credential": credential });
    match core.submit(None, None, Upstream::Hello { auth: Some(auth), context: None }) {
        Ok(Reply::Hello { connection }) => connection,
        other => panic!("hello_member: {other:?}"),
    }
}

/// Run the public login and return the minted cose-token wire object.
fn login_token<P: KeyProvider>(core: &mut ConnectCore<MemoryStore, P>, conn: &ConnectionToken) -> Json {
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

/// Submit a `view` on `address` and return the reply (opened, or a refusal).
fn view<P: KeyProvider>(core: &mut ConnectCore<MemoryStore, P>, conn: &ConnectionToken, address: &str) -> Reply {
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

/// §17.7/§17.8 happy path: mint over the injected ring, authenticate with the
/// returned token through the connector's cose gate, and read the role view; an
/// unauthenticated connection is denied the same view.
#[test]
fn native_cose_roundtrip_authenticates() {
    let mut core = injected_core();

    let anon = hello_anon(&mut core);
    let token = login_token(&mut core, &anon);

    // The authenticated connection reads the member-only roster.
    let member = hello_member(&mut core, &token);
    assert!(
        matches!(view(&mut core, &member, "member.roster"), Reply::Opened { .. }),
        "the cose-authenticated member reads the role view",
    );

    // Control: an anonymous connection is denied the role view.
    assert!(
        matches!(view(&mut core, &anon, "member.roster"), Reply::Outcome(Outcome::Denied { .. })),
        "an unauthenticated connection cannot read the role view",
    );
}

/// A tampered token (its `$claims` altered after signing) fails the injected
/// ring's signature check, so the gate yields the `none` sentinel and the member
/// role never binds — the connection stays anonymous and is denied the role view.
#[test]
fn tampered_token_is_denied() {
    let mut core = injected_core();
    let anon = hello_anon(&mut core);
    let mut token = login_token(&mut core, &anon);

    // Alter a signed claim; the genuine signature no longer matches the payload.
    token["$claims"]["session"] = json!("00000000-0000-0000-0000-000000000000");
    let member = hello_member(&mut core, &token);
    assert!(
        matches!(view(&mut core, &member, "member.roster"), Reply::Outcome(Outcome::Denied { .. })),
        "a tampered cose token does not authenticate",
    );
}

/// A token minted by a SIM-backed engine is denied by the injected core: its
/// signature does not verify under the injected ring's Ed25519 key. This is the
/// security property Task 5 restores — a forgeable sim-signed token cannot
/// authenticate against a deployment signing with real keys.
#[test]
fn sim_signed_token_is_denied() {
    // Mint a token on a sim-backed core (no registered provider).
    let mut sim: ConnectCore<MemoryStore> = mount("cose-sim", None);
    let sim_anon = hello_anon(&mut sim);
    let sim_token = login_token(&mut sim, &sim_anon);

    // Present it to the injected-Ed25519 core.
    let mut core = injected_core();
    let member = hello_member(&mut core, &sim_token);
    assert!(
        matches!(view(&mut core, &member, "member.roster"), Reply::Outcome(Outcome::Denied { .. })),
        "a sim-signed token cannot authenticate against the injected ring",
    );
}
