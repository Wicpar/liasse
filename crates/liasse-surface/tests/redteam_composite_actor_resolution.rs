#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probe (§11.1/§11.3, §5.4, §10.3): a COMPOSITE-keyed `$actor`
//! collection cannot be authenticated through the surface layer, even though the
//! engine explicitly supports a composite `$actor` binding.
//!
//! The engine.rs:1299 `$actor`/`$session` fix states the actor row's key "is its
//! application-visible identity — a positional `Value::Composite` when the
//! actor/session collection is composite-keyed" and routes it through
//! `key_value_of` so the binding "addresses the stored N-component row". The
//! sibling engine probe `redteam_composite_actor_binding` proves the engine does
//! exactly that when handed a `Value::Composite` actor key.
//!
//! But the surface authentication layer never produces that value. An actor's
//! identity is `RowSource::key_field` — a SINGLE projected field
//! (`SessionAuthenticator::actor`, `crates/liasse-surface/src/authn/session.rs`),
//! and a `RowSource` resolves a row by comparing that ONE field
//! (`match_rows`, `crates/liasse-surface/src/authn/identity.rs`). A composite
//! `$key: [org, user]` (§5.4) has no single field that is the whole key, so:
//!
//! - the actor resolution compares one component against the full identity and
//!   never matches (authentication denies), and even if it did match,
//! - the resulting `Actor::key` would carry a single component, so the engine
//!   re-materializes `$actor` at a one-component address that names no stored
//!   composite row and the admitted program reading `$actor` faults closed.
//!
//! §11.3 requires `$actor` to resolve exactly one row; §5.4 makes a composite
//! account row a spec-legal actor uniquely identified by its ordered tuple; §10.3
//! authorizes that actor. The composite account below EXISTS, is enabled, and is
//! uniquely identified by `[acme, alice]`, so authentication MUST bind and the
//! role call MUST commit with `$actor` bound to it. The single-keyed CONTROL
//! (identical shape, one-field key) proves the harness, wiring, and role plumbing
//! are correct — isolating the failure to composite-key actor resolution alone.

mod support;

use liasse_ident::InstanceId;
use liasse_store::MemoryStore;
use liasse_surface::{
    Authenticate, AuthResult, AuthSelection, CallBinding, Claims, Credential, Engine, Precision,
    Role, RowSource, SessionAuthenticator, SurfaceBinding, SurfaceCall, SurfaceHost, SurfaceOutcome,
    SurfaceRouter, SurfaceRouterBuilder, Value, Verifier, VerifyFailure, VirtualClock,
};
use support::{address, args, text, NOW};

/// A verifier that binds its proof to `auth` and returns a fixed account claim —
/// the value the surface then resolves `$actor` against. The claim can be a
/// scalar (control) or a positional `Value::Composite` (§5.4 composite identity).
struct FixedAccountVerifier {
    auth: String,
    account: Value,
}

impl Verifier for FixedAccountVerifier {
    fn verify(&self, credential: &Credential) -> Result<Claims, VerifyFailure> {
        // A well-formed text credential is accepted; the account it selects is
        // fixed by the authenticator wiring (a stand-in for a real `$verify`).
        match credential.value() {
            Value::Text(_) => Ok(Claims::new(&self.auth, None, Some(self.account.clone()))),
            _ => Err(VerifyFailure::new("credential is not a token")),
        }
    }
}

/// The composite actor identity `[org, user]` in `$key` order (§5.4).
fn composite(org: &str, user: &str) -> Value {
    Value::Composite(vec![text(org), text(user)])
}

// ---------------------------------------------------------------------------
// Composite-keyed `$actor` collection (`$key: [org, user]`).
// ---------------------------------------------------------------------------

const COMPOSITE_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.compactor@1.0.0"
  "$model": {
    "accounts": {
      "$key": ["org", "user"]
      "org": "text"
      "user": "text"
      "enabled": "bool = true"
    }
    "accounts_view": { "$view": ".accounts { org, user, enabled }" }
    "members_view": { "$view": ".accounts[:a | a.enabled] { org, user }" }
    "$mut": {
      "whoami": "return $actor { org, user }"
    }
    "$auth": {
      "api": {
        "$credential": "text"
        "$verify": "token.verify($credential)"
        "$actor": "/accounts[{ org: $proof.org, user: $proof.user }]"
      }
    }
    "$roles": {
      "member": {
        "$auth": "api"
        "$members": ".members_view"
        "me": { "$mut": { "whoami": ".whoami" } }
      }
    }
  }
  "$data": {
    "accounts": { "acme:alice": { "org": "acme", "user": "alice" } }
  }
}"#;

fn composite_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = match Engine::load(MemoryStore::new(InstanceId::new("compactor")), COMPOSITE_APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    };
    let router = composite_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn composite_router(model: &liasse_model::Model) -> SurfaceRouter {
    // The actor is the composite account `[acme, alice]`. A `RowSource` can name
    // only ONE key field, so the wiring is forced to pick a single component of a
    // two-component key — the very gap under test.
    let api = SessionAuthenticator::stateless(
        "api",
        Box::new(FixedAccountVerifier { auth: "api".to_owned(), account: composite("acme", "alice") }),
        RowSource::new("accounts_view", "org"),
    );
    let member = Role::new("member", ["api".to_owned()], RowSource::new("members_view", "org"));
    let me = SurfaceBinding::new().with_call("whoami", CallBinding::root("whoami", []));
    SurfaceRouterBuilder::new()
        .authenticator(Box::new(api))
        .role(member, [("me".to_owned(), me)])
        .build(model)
        .expect("router validates against the model")
}

#[test]
fn composite_keyed_actor_authenticates_and_binds() {
    // §5.4/§11.3: the composite account `[acme, alice]` exists, is enabled, and is
    // the uniquely identified actor. Authentication MUST bind it (§11.3
    // exactly-one), and the role call MUST commit with `$actor` resolved to that
    // composite row, returning `{ org: acme, user: alice }` (§11.1).
    let mut host = composite_host();
    host.connect("c1").unwrap();
    let auth = host
        .authenticate(
            "c1",
            &Authenticate::new("member", AuthSelection::new("api", Credential::new(text("acme:alice")))),
        )
        .expect("authenticate");
    assert!(
        matches!(auth, AuthResult::Bound),
        "§11.3/§5.4: the composite account `[acme, alice]` is a uniquely identified actor and MUST \
         authenticate; the surface truncates its identity to one `key_field` component and denies: {auth:?}",
    );

    let outcome = host
        .call("c1", &SurfaceCall::new(address("member.me.whoami"), args([])))
        .expect("call");
    // A `return $actor {...}` mutates nothing, so §8.9 delivers `unchanged` — both
    // success completions carry the projected actor row.
    let response = match outcome {
        SurfaceOutcome::Unchanged { response, .. } | SurfaceOutcome::Committed { response, .. } => response,
        other => panic!(
            "§11.1: an authenticated composite-keyed actor must admit a `$actor`-reading mutation, got {other:?}"
        ),
    };
    let wire = response.expect("a return value").to_wire();
    assert_eq!(wire.get("org").and_then(|v| v.as_str()), Some("acme"), "$actor.org");
    assert_eq!(
        wire.get("user").and_then(|v| v.as_str()),
        Some("alice"),
        "§5.4: $actor resolves the full composite row, so its second key component reads back",
    );
}

// ---------------------------------------------------------------------------
// CONTROL: identical shape with a single-field key. Proves the wiring is sound
// so the composite failure above is attributable to composite-key resolution.
// ---------------------------------------------------------------------------

const SCALAR_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.scalaractor@1.0.0"
  "$model": {
    "accounts": {
      "$key": "id"
      "id": "text"
      "enabled": "bool = true"
    }
    "accounts_view": { "$view": ".accounts { id, enabled }" }
    "members_view": { "$view": ".accounts[:a | a.enabled] { id }" }
    "$mut": {
      "whoami": "return $actor { id }"
    }
    "$auth": {
      "api": {
        "$credential": "text"
        "$verify": "token.verify($credential)"
        "$actor": "/accounts[$proof.account]"
      }
    }
    "$roles": {
      "member": {
        "$auth": "api"
        "$members": ".members_view"
        "me": { "$mut": { "whoami": ".whoami" } }
      }
    }
  }
  "$data": {
    "accounts": { "alice": { } }
  }
}"#;

fn scalar_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = match Engine::load(MemoryStore::new(InstanceId::new("scalaractor")), SCALAR_APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    };
    let router = scalar_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn scalar_router(model: &liasse_model::Model) -> SurfaceRouter {
    let api = SessionAuthenticator::stateless(
        "api",
        Box::new(FixedAccountVerifier { auth: "api".to_owned(), account: text("alice") }),
        RowSource::new("accounts_view", "id"),
    );
    let member = Role::new("member", ["api".to_owned()], RowSource::new("members_view", "id"));
    let me = SurfaceBinding::new().with_call("whoami", CallBinding::root("whoami", []));
    SurfaceRouterBuilder::new()
        .authenticator(Box::new(api))
        .role(member, [("me".to_owned(), me)])
        .build(model)
        .expect("router validates against the model")
}

#[test]
fn scalar_keyed_actor_authenticates_and_binds_control() {
    // CONTROL: the same shape with a single-field key admits and reads `$actor`,
    // proving the harness, actor resolution, role membership, and `$actor`
    // threading are correct — so the composite failure is the composite-key gap.
    let mut host = scalar_host();
    host.connect("c1").unwrap();
    let auth = host
        .authenticate(
            "c1",
            &Authenticate::new("member", AuthSelection::new("api", Credential::new(text("alice")))),
        )
        .expect("authenticate");
    assert!(matches!(auth, AuthResult::Bound), "scalar actor authenticates: {auth:?}");

    let outcome = host
        .call("c1", &SurfaceCall::new(address("member.me.whoami"), args([])))
        .expect("call");
    let response = match outcome {
        SurfaceOutcome::Unchanged { response, .. } | SurfaceOutcome::Committed { response, .. } => response,
        other => panic!("scalar actor must admit the `$actor`-reading mutation, got {other:?}"),
    };
    let wire = response.expect("a return value").to_wire();
    assert_eq!(wire.get("id").and_then(|v| v.as_str()), Some("alice"), "$actor.id reads back");
}
