#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! PROBE: §10.3 ref-typed `$members`. The spec's own §10.3 `companies` example
//! declares `$members: ".members[:m | m.admin].account"` where `account` is
//! `{ "$ref": "/accounts" }`, so the membership view projects a `Value::Ref`.
//! §10.3: "The actor holds the role when its exact row identity occurs at least
//! once in the resulting view." §5.6: a ref's application value is its target's
//! typed key, so a ref member whose target is the actor's account row IS that
//! actor's row identity. Membership must therefore match the scalar actor key.

mod support;

use liasse_ident::InstanceId;
use liasse_store::MemoryStore;
use liasse_surface::{
    Authenticate, AuthResult, AuthSelection, CallBinding, Credential, Engine, Precision, Role,
    RowSource, SessionAuthenticator, SurfaceBinding, SurfaceHost, SurfaceOutcome, SurfaceRouter,
    SurfaceRouterBuilder, ViewBinding, VirtualClock,
};
use support::{address, args, text, TokenVerifier, NOW};

/// Accounts, and a `company_members` collection keyed by a *ref* to an account
/// (exactly §10.3's / §11.2's ref shape). `members_view` projects that ref column,
/// so the role membership view yields `Value::Ref` rows.
const APP: &str = r#"{
  "$liasse": 1
  "$app": "example.refmembers@1.0.0"
  "$model": {
    "accounts": { "$key": "id", "id": "text", "enabled": "bool = true" }
    "company_members": {
      "$key": "account"
      "account": { "$ref": "/accounts" }
    }
    "accounts_view": { "$view": ".accounts { id, enabled }" }
    "members_view": { "$view": ".company_members { account }" }
    "scalar_members_view": { "$view": ".accounts { id }" }
    "$mut": {
      "touch": ".accounts[@id].enabled = false"
    }
    "$auth": {
      "api": {
        "$credential": "text"
        "$verify": "token.verify($credential)"
        "$actor": "/accounts[$proof.account]"
        "$check": "$proof.auth == $auth_name"
      }
    }
    "$roles": {
      "member": {
        "$auth": "api"
        "$members": ".members_view"
        "admin": { "$view": ".accounts_view", "$mut": { "touch": ".touch" } }
      }
      "plain": {
        "$auth": "api"
        "$members": ".scalar_members_view"
        "admin": { "$view": ".accounts_view", "$mut": { "touch": ".touch" } }
      }
    }
  }
  "$data": {
    "accounts": { "alice": { } }
    "company_members": { "alice": { } }
  }
}"#;

fn probe_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = match Engine::load(MemoryStore::new(InstanceId::new("refmembers")), APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    };
    let router = router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn router(model: &liasse_model::Model) -> SurfaceRouter {
    let admin = SurfaceBinding::new()
        .with_view(ViewBinding::new("accounts_view"))
        .with_call("touch", CallBinding::root("touch", ["id".to_owned()]));
    let api = SessionAuthenticator::stateless(
        "api",
        Box::new(TokenVerifier::new("api", false)),
        RowSource::new("accounts_view", "id"),
    );
    // Members are read from the ref-projecting membership view, keyed by `account`.
    let member = Role::new("member", ["api".to_owned()], RowSource::new("members_view", "account"));
    // Control: identical role but scalar membership projection.
    let plain_admin = SurfaceBinding::new()
        .with_view(ViewBinding::new("accounts_view"))
        .with_call("touch", CallBinding::root("touch", ["id".to_owned()]));
    let plain = Role::new("plain", ["api".to_owned()], RowSource::new("scalar_members_view", "id"));
    SurfaceRouterBuilder::new()
        .authenticator(Box::new(api))
        .role(member, [("admin".to_owned(), admin)])
        .role(plain, [("admin".to_owned(), plain_admin)])
        .build(model)
        .expect("router validates against the model")
}

#[test]
fn ref_typed_member_holds_the_role() {
    // alice's account ref sits in `company_members`, so alice IS a member (§10.3).
    // Membership compares the actor's row identity (§5.6 application key), so the
    // ref member must match the scalar actor key `alice` and the call must commit.
    let mut host = probe_host();
    host.connect("c1").unwrap();
    let auth = host
        .authenticate(
            "c1",
            &Authenticate::new("member", AuthSelection::new("api", Credential::new(text("alice")))),
        )
        .expect("authenticate");
    assert!(matches!(auth, AuthResult::Bound), "the api actor authenticates: {auth:?}");

    let outcome = host
        .call("c1", &liasse_surface::SurfaceCall::new(address("member.admin.touch"), args([("id", text("alice"))])))
        .expect("call");
    assert!(
        matches!(outcome, SurfaceOutcome::Committed { .. }),
        "a ref-typed membership must admit the member (§10.3), got {outcome:?}",
    );
}

#[test]
fn scalar_member_holds_the_role_control() {
    // CONTROL: the exact same actor and app, but membership projects a scalar `id`
    // column instead of a ref. This admits — proving the harness, the actor
    // resolution, and the role wiring are all correct, and isolating the failure
    // above to the ref-vs-scalar membership comparison alone.
    let mut host = probe_host();
    host.connect("c1").unwrap();
    let auth = host
        .authenticate(
            "c1",
            &Authenticate::new("plain", AuthSelection::new("api", Credential::new(text("alice")))),
        )
        .expect("authenticate");
    assert!(matches!(auth, AuthResult::Bound), "the api actor authenticates: {auth:?}");

    let outcome = host
        .call("c1", &liasse_surface::SurfaceCall::new(address("plain.admin.touch"), args([("id", text("alice"))])))
        .expect("call");
    assert!(
        matches!(outcome, SurfaceOutcome::Committed { .. }),
        "a scalar membership admits the same actor, got {outcome:?}",
    );
}
