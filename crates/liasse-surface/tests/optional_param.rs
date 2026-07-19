#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.3/§A.1 surface argument binding: a declared mutation parameter the caller
//! omits is not rejected at the surface. An omitted optional parameter binds the
//! absent value `none` in the runtime (clearing the field, §8.5); an omitted
//! *required* parameter is still rejected — by the runtime, not pre-empted by the
//! surface. The surface layer therefore passes an omitted parameter through
//! rather than demanding every declared parameter be supplied.

mod support;

use liasse_ident::InstanceId;
use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Precision, SurfaceAddress, SurfaceBinding, SurfaceCall, SurfaceHost,
    SurfaceOutcome, SurfaceRouterBuilder, Value, ViewBinding, VirtualClock,
};
use liasse_value::Text;

/// A model with an optional field and a mutation that assigns it from a
/// parameter, so an omitted argument clears the field (§8.5).
const OPT_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.optparam@1.0.0"
  "$model": {
    "profiles": { "$key": "id", "id": "text", "email": "text?" }
    "index": { "$view": ".profiles { id, email, $sort: [id] }" }
    "$mut": { "set_email": ".profiles[@id].email = @email" }
    "$public": {
      "profiles": { "$view": ".index", "$mut": { "set_email": ".set_email" } }
    }
  }
  "$data": { "profiles": { "p1": { "email": "a@x" } } }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// A host over [`OPT_APP`] whose `public.profiles.set_email` binds both the `id`
/// and `email` parameters (the full external contract).
fn host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(1_700_000_000_000_000, Precision::Micros);
    let store = MemoryStore::new(InstanceId::new("optparam"));
    let engine = match Engine::load(store, OPT_APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    };
    let profiles = SurfaceBinding::new().with_view(ViewBinding::new("index")).with_call(
        "set_email",
        CallBinding::root("set_email", ["id".to_owned(), "email".to_owned()]),
    );
    let router = SurfaceRouterBuilder::new()
        .public_surface("profiles", profiles)
        .build(engine.model())
        .expect("router validates against the model");
    SurfaceHost::new(engine, router, clock)
}

/// `p1`'s stored `email`, or `None` when the field is absent.
fn email_of(host: &SurfaceHost<MemoryStore>, id: &str) -> Option<Value> {
    let view = host.engine().view_at_head("index").expect("view").expect("declared");
    let row = view.rows().iter().find(|row| row.field("id") == Some(&text(id))).expect("row present");
    row.field("email").cloned()
}

fn call(id: &str, email: Option<&str>) -> SurfaceCall {
    let mut args = std::collections::BTreeMap::new();
    args.insert("id".to_owned(), text(id));
    if let Some(email) = email {
        args.insert("email".to_owned(), text(email));
    }
    SurfaceCall::new(SurfaceAddress::parse("public.profiles.set_email").expect("address"), args)
}

#[test]
fn omitted_optional_parameter_passes_through_and_clears_the_field() {
    // §8.3/§A.1: omitting the optional `@email` argument must not be rejected at
    // the surface; it binds `none` and clears the stored field (§8.5).
    let mut host = host();
    host.connect("c1").unwrap();
    assert_eq!(email_of(&host, "p1"), Some(text("a@x")), "seed email is present");

    let outcome = host.call("c1", &call("p1", None)).expect("call");
    assert!(
        matches!(outcome, SurfaceOutcome::Committed { .. }),
        "the surface admits a call omitting an optional parameter: {outcome:?}"
    );
    assert_eq!(email_of(&host, "p1"), None, "the omitted optional argument cleared the field");
}

#[test]
fn supplied_optional_parameter_sets_the_field() {
    // The contrast case: supplying the argument stores the value, so the omitted
    // path is a genuine state change, not a no-op.
    let mut host = host();
    host.connect("c1").unwrap();
    let outcome = host.call("c1", &call("p1", Some("b@y"))).expect("call");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "supplied argument commits: {outcome:?}");
    assert_eq!(email_of(&host, "p1"), Some(text("b@y")), "the supplied argument was stored");
}

#[test]
fn omitted_required_parameter_is_still_rejected() {
    // Passing omitted parameters through does not weaken required-parameter
    // enforcement: `@id` is non-optional, so omitting it is rejected downstream
    // rather than silently defaulted.
    let mut host = host();
    host.connect("c1").unwrap();
    let args = {
        let mut map = std::collections::BTreeMap::new();
        map.insert("email".to_owned(), text("b@y"));
        map
    };
    let address = SurfaceAddress::parse("public.profiles.set_email").expect("address");
    let outcome = host.call("c1", &SurfaceCall::new(address, args)).expect("call");
    assert!(
        outcome.rejection().is_some(),
        "an omitted required parameter is still rejected: {outcome:?}"
    );
    assert_eq!(email_of(&host, "p1"), Some(text("a@x")), "the rejected call changed no state");
}
