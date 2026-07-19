#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 / §10.1 / §8.3 resume of a *parameterized* live subscription.
//!
//! A subscription opened over a parameterized surface `$view` reads its `@param`
//! bindings from the client-supplied surface arguments (§10.1); an omitted
//! parameter takes its declared default (§8.3). §12.2 says resuming from a
//! retained frontier "yields the later authorized patches in that stream or a
//! fresh `init`" — *that* stream, i.e. the same authorized declared view the
//! subscription was opened with. So resuming a stream that was opened filtered to
//! `owner == "alice"` must reconstruct the alice-filtered view, not silently
//! collapse to the declared default (`owner == "anon"`), which is a different
//! result set. A resume therefore carries the same surface arguments the original
//! subscription was opened with; the retained frontier alone does not encode them.

mod support;

use std::collections::BTreeMap;

use liasse_ident::InstanceId;
use liasse_store::MemoryStore;
use liasse_surface::{
    Engine, Precision, Subscription, SurfaceAddress, SurfaceBinding, SurfaceHost, SurfaceResume,
    SurfaceRouterBuilder, SurfaceWatch, Value, ViewBinding, ViewResult, VirtualClock,
};
use liasse_value::Text;

/// A parameterized public surface view: `owned` filters `.tasks` to the owner the
/// `@owner` parameter names, defaulting to `'anon'` when the argument is omitted
/// (§8.3). Three tasks are seeded, one per owner, so each owner value selects a
/// distinct single row — the default (`anon`) and the alice filter are provably
/// different result sets.
const PARAM_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.paramview@1.0.0"
  "$model": {
    "tasks": { "$key": "id", "id": "text", "owner": "text = 'anon'" }
    "$mut": { "add": ".tasks + { id: @id, owner: @owner }" }
    "$public": {
      "owned": {
        "$params": { "owner": "text = 'anon'" }
        "$view": ".tasks[:t | t.owner == @owner] { id, owner, $sort: [id] }"
        "$mut": { "add": ".add" }
      }
    }
  }
  "$data": {
    "tasks": {
      "t1": { "owner": "alice" }
      "t2": { "owner": "bob" }
      "t3": { "owner": "anon" }
    }
  }
}"#;

/// A text value.
fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// Parse a dotted surface address.
fn address(target: &str) -> SurfaceAddress {
    SurfaceAddress::parse(target).expect("address parses")
}

/// The `{ owner: o }` surface-argument map a parameterized watch/resume supplies.
fn owner_arg(owner: &str) -> BTreeMap<String, Value> {
    let mut args = BTreeMap::new();
    args.insert("owner".to_owned(), text(owner));
    args
}

/// The `id` column of a view result, in order.
fn ids(result: &ViewResult) -> Vec<String> {
    result
        .rows()
        .iter()
        .map(|row| match row.field("id") {
            Some(Value::Text(t)) => t.as_str().to_owned(),
            other => panic!("unexpected id cell {other:?}"),
        })
        .collect()
}

/// A host over [`PARAM_APP`] whose `public.owned` binds the parameterized surface
/// view (evaluated with `$params` in scope) and its `add` mutation.
fn probe_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(1_700_000_000_000_000, Precision::Micros);
    let store = MemoryStore::new(InstanceId::new("paramview"));
    let engine = match Engine::load(store, PARAM_APP, &mut clock) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    };
    let owned = SurfaceBinding::new().with_view(ViewBinding::surface("public.owned"));
    let router = SurfaceRouterBuilder::new()
        .public_surface("owned", owned)
        .build(engine.model())
        .expect("router validates against the model");
    SurfaceHost::new(engine, router, clock)
}

/// Open a watch with `args` and return the row ids of its initial result.
fn open(host: &mut SurfaceHost<MemoryStore>, conn: &str, watch: &SurfaceWatch) -> Vec<String> {
    match host.watch(conn, watch).expect("watch") {
        Subscription::Init(result) => ids(&result),
        other => panic!("expected an init, got {other:?}"),
    }
}

#[test]
fn parameterized_view_filters_by_supplied_arg() {
    // §10.1: a fresh watch supplying `owner = "alice"` binds `@owner` to alice, so
    // the result is the alice-owned row alone. This isolates the resume defect: a
    // fresh parameterized read already honors the argument.
    let mut host = probe_host();
    host.connect("c1").unwrap();
    let watch = SurfaceWatch::new(address("public.owned"), "w1").with_args(owner_arg("alice"));
    assert_eq!(open(&mut host, "c1", &watch), ["t1"], "the alice arg filters to alice's row");
}

#[test]
fn default_param_selects_the_declared_default_owner() {
    // §8.3: omitting `owner` binds `@owner` to its declared default `'anon'`, a
    // provably different result set (t3, not t1) — so the alice filter carries
    // information that must survive a resume.
    let mut host = probe_host();
    host.connect("c1").unwrap();
    let watch = SurfaceWatch::new(address("public.owned"), "w1");
    assert_eq!(open(&mut host, "c1", &watch), ["t3"], "the default arg filters to anon's row");
}

#[test]
fn resume_of_a_parameterized_subscription_preserves_its_filter() {
    // §12.2: "Resuming from a retained frontier yields the later authorized patches
    // in that stream." The stream was opened filtered to owner == "alice", so its
    // resumed reconstruction must still be the alice-filtered view (t1), never the
    // default 'anon' result set (t3). The client re-supplies the same surface
    // arguments; the retained frontier alone does not encode them.
    let mut host = probe_host();
    host.connect("c1").unwrap();
    let watch = SurfaceWatch::new(address("public.owned"), "w1").with_args(owner_arg("alice"));
    assert_eq!(open(&mut host, "c1", &watch), ["t1"], "the opened stream is alice-filtered");
    let from = host.frontier("c1").expect("connection open");
    host.disconnect("c1");

    host.connect("c2").unwrap();
    let resume = SurfaceResume::new(address("public.owned"), "w2", from).with_args(owner_arg("alice"));
    match host.resume("c2", &resume).expect("resume") {
        Subscription::Init(result) => assert_eq!(
            ids(&result),
            ["t1"],
            "the resumed stream must keep the alice filter, not reset to the default 'anon'",
        ),
        other => panic!("expected a reconstructed init, got {other:?}"),
    }
}
