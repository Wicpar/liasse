//! RED TEAM — §10.4 uniform-denial oracle probe for the scoped-role coverage
//! surface (§10.3/§10.5, F6). The scenario harness collapses every refusal to one
//! `denied` class, so it cannot see a MESSAGE-level distinction. This test drives
//! the real [`SurfaceHost`] directly and inspects the exact [`Denial`] (`reason`
//! AND `message`) each scoped watch refusal carries.
//!
//! §10.4 (and the code's own contract, `hide_unenumerable_denial`) require every
//! denial a caller is "not authorized to have served" to be indistinguishable —
//! same class, code, AND message — from a nonexistent name, so membership/scope
//! existence cannot be enumerated by the wire. A pair that differs is an oracle.
//!
//! Expectations are deducible from SPEC.md §10.3/§10.4/§10.5 alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use liasse_ident::InstanceId;
use liasse_store::MemoryStore;
use liasse_surface::{
    Claims, Credential, Denial, DenialReason, Engine, Precision, Role, RowSource,
    SessionAuthenticator, Subscription, SurfaceAddress, SurfaceBinding, SurfaceHost, SurfaceRouter,
    SurfaceRouterBuilder, SurfaceWatch, Value, Verifier, VerifyFailure, ViewBinding, VirtualClock,
};
use liasse_value::Text;

const NOW: i128 = 1_700_000_000_000_000;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// A stateless token verifier that echoes the credential text as the account key
/// (§11.3 stand-in), so credential `"alice"` resolves actor `accounts[alice]`.
struct AccountVerifier;

impl Verifier for AccountVerifier {
    fn verify(&self, credential: &Credential) -> Result<Claims, VerifyFailure> {
        let Value::Text(_) = credential.value() else {
            return Err(VerifyFailure::new("credential is not a token"));
        };
        Ok(Claims::new("token", None, Some(credential.value().clone())))
    }
}

/// A scoped-role package (§10.3/§10.5): `companies` carries a self-referential
/// `subcompanies` relation and an `admin` role scoped per company row, propagating
/// the `. { id, name, plan }` surface through `subcompanies` as `$recursive`
/// coverage. `alice` is admin of `acme`; `carol` is a bystander account.
const SCOPED_APP: &str = r#"{
  "$liasse": 1,
  "$app": "example.scoped@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text" },
    "companies": {
      "$key": "id", "id": "text", "name": "text", "plan": "text = 'active'",
      "subcompanies": { "$like": "^" },
      "members": { "$key": "account", "account": { "$ref": "/accounts" }, "admin": "bool = false" },
      "$roles": {
        "admin": {
          "$auth": "token",
          "$members": ".members[:m | m.admin].account",
          "company": {
            "$view": ". { id, name, plan }",
            "$recursive": { "$field": "subcompanies", "$through": ".subcompanies", "$bind": "child" }
          }
        }
      }
    },
    "accounts_view": { "$view": ".accounts { id }" },
    "role_members": { "$view": ".companies[:c].members[:m | m.admin] { scope_row: c.id, account: m.account }" },
    "$auth": {
      "token": {
        "$credential": "text",
        "$verify": "token.verify($credential)",
        "$actor": "/accounts[$proof.account]",
        "$check": "$proof.auth == $auth_name"
      }
    }
  },
  "$data": {
    "accounts": { "alice": {}, "carol": {} },
    "companies": {
      "acme": { "name": "Acme", "members": { "alice": { "admin": true } } }
    }
  }
}"#;

/// Build a host wiring the scoped `admin` role over [`SCOPED_APP`].
fn host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let store = MemoryStore::new(InstanceId::new("scoped"));
    let engine = Engine::load(store, SCOPED_APP, &mut clock).expect("scoped app loads");
    let router = router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn router(model: &liasse_model::Model) -> SurfaceRouter {
    let token = SessionAuthenticator::stateless(
        "token",
        Box::new(AccountVerifier),
        RowSource::new("accounts_view", "id"),
    );
    // §10.3: a SCOPED role whose membership view projects each grant's
    // role-holding-row key under `scope_row`, so membership is confirmed per row.
    let admin = Role::scoped(
        "admin",
        ["token".to_owned()],
        RowSource::new("role_members", "account"),
        "scope_row",
    );
    let company = SurfaceBinding::new().with_view(ViewBinding::surface("admin.company"));
    SurfaceRouterBuilder::new()
        .authenticator(Box::new(token))
        .role(admin, [("company".to_owned(), company)])
        .build(model)
        .expect("router validates against the scoped model")
}

/// Open a connection and watch `admin.company` under `scope`, authenticating inline
/// as `credential`. Returns the refusal, or panics if the watch unexpectedly opened.
fn watch_denial(scope: &[Value], credential: &str) -> Denial {
    let mut host = host();
    host.connect("c1");
    let mut watch = SurfaceWatch::new(SurfaceAddress::parse("admin.company").expect("addr"), "w1")
        .with_auth(liasse_surface::AuthSelection::new("token", Credential::new(text(credential))));
    if !scope.is_empty() {
        watch = watch.with_scope(scope.iter().cloned());
    }
    match host.watch("c1", &watch).expect("watch runs") {
        Subscription::Denied(denial) => denial,
        other => panic!("expected a denial for scope {scope:?} cred {credential}, got {other:?}"),
    }
}

/// Control: the legitimate holder (alice, admin of acme) opens the coverage view
/// for her own scope row. Establishes the deny paths below are boundaries, not a
/// dead surface.
#[test]
fn legitimate_scope_watch_opens() {
    let mut host = host();
    host.connect("c1");
    let watch = SurfaceWatch::new(SurfaceAddress::parse("admin.company").expect("addr"), "w1")
        .with_scope([text("acme")])
        .with_auth(liasse_surface::AuthSelection::new("token", Credential::new(text("alice"))));
    match host.watch("c1", &watch).expect("watch runs") {
        Subscription::Init(_) => {}
        other => panic!("expected the covered view to open, got {other:?}"),
    }
}

/// §10.4: a non-member (carol) and a nonexistent scope (`ghost`, held by the real
/// member alice) MUST deny with the identical class, code, AND message — no
/// membership/existence oracle. This pins the baseline uniform denial the
/// remaining probes are compared against.
#[test]
fn nonmember_and_nonexistent_scope_are_identical_denials() {
    let nonmember = watch_denial(&[text("acme")], "carol");
    let ghost_scope = watch_denial(&[text("ghost")], "alice");

    assert_eq!(
        nonmember.reason(),
        DenialReason::Unresolved,
        "a non-member scoped watch denies `unresolved` (§10.4)",
    );
    assert_eq!(
        (ghost_scope.reason(), ghost_scope.message()),
        (nonmember.reason(), nonmember.message()),
        "§10.4: a nonexistent scope must be indistinguishable (class, code, AND \
         message) from a non-member — nonmember={nonmember:?} ghost={ghost_scope:?}",
    );
}

/// FINDING (HELD REPRO, `#[ignore]`d) — §10.4 empty-scope denial oracle.
///
/// A scoped watch that OMITS the scope key is authorized by the "holds the role
/// under ANY row" manifest question (`role.rs:138` `scope.first()` → the `_`
/// branch → `RowSource::contains`), so a member-somewhere (alice) PASSES
/// membership and is refused only downstream, when the covered view finds no row
/// to materialize (`recursion.rs:100` `key_of([]) == None`). That refusal carries
/// message "the surface view is not declared" (`call.rs:511-514`), whereas a
/// non-member (carol) is refused up front by the uniform unresolvable-name denial
/// "the address names nothing exposed to this caller" (`call.rs:278-280`).
///
/// Both are `DenialReason::Unresolved`, but the MESSAGE differs, so an empty-scope
/// probe distinguishes "you hold this role somewhere" from "you do not" — a
/// self-membership oracle the §10.4 contract (`hide_unenumerable_denial`:
/// "identical in class, code, AND message") forbids. Severity is bounded: it
/// leaks only a boolean about the CALLER'S OWN role holding (not another actor's
/// data, and not scope existence — a nonexistent scope for a real member gives the
/// uniform message, pinned green by `nonmember_and_nonexistent_scope_are_identical_denials`),
/// under a degenerate empty-scope input, and only where the transport forwards the
/// diagnostic message rather than the stable `reason` code alone.
///
/// This asserts the SPEC-CORRECT result (identical message). It is now a GREEN
/// regression lock: the fix rejects an empty scope on a scoped role up front to the
/// uniform `unresolved_name` denial (`role.rs` no longer falls the empty-scope arm
/// through to the any-row `RowSource::contains`), and routes the covered-view
/// materialize-`None` branch through `SurfaceHost::unresolved_name`
/// (`call.rs`) instead of minting its own message.
#[test]
fn empty_scope_watch_denial_is_uniform() {
    let member_empty = watch_denial(&[], "alice");
    let nonmember_empty = watch_denial(&[], "carol");

    // Both refusals are the same coarse class (this part already holds).
    assert_eq!(member_empty.reason(), DenialReason::Unresolved);
    assert_eq!(nonmember_empty.reason(), DenialReason::Unresolved);

    // SPEC-correct: §10.4 requires the two refusals to be indistinguishable in
    // class, code, AND message. The fix makes them identical — this locks it.
    assert_eq!(
        member_empty.message(),
        nonmember_empty.message(),
        "§10.4 oracle: an empty-scope member and non-member must deny with the \
         SAME message — member={member_empty:?} nonmember={nonmember_empty:?}",
    );
}
