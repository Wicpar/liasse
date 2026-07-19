#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 live-view coherence for a SCALAR / AGGREGATE view.
//!
//! §7.5 defines aggregate views that deliver a single scalar (`count(view) ->
//! int`, `= size(.docs)`, ...). The runtime models this first-class: a `$view`
//! whose body is an aggregate evaluates to [`ViewResult::Scalar`] ("A
//! scalar/aggregate view stays a value.", engine.rs), a public surface may expose
//! it (`liasse-model` behavior corpus: `"n": { "$view": "count(.items)" }`), and
//! `Subscription::Init` returns that full `ViewResult` including the scalar.
//!
//! §12.2 fixes the live-view contract for EVERY subscribed view: it begins with a
//! complete result and a frontier, then receives ordered patches, and "After
//! applying every operation, the client result MUST equal the authorized declared
//! view at the new frontier." The declared view of a `count` surface at a frontier
//! is the integer count; a commit that changes the count must be conveyed so the
//! client result reaches the new count.
//!
//! These probes drive real commits, snapshot the authorized scalar view, and ask
//! the SAME §12.2 delta primitive the row cases use — `ViewDelta::between` — to
//! carry a faithful client from the prior scalar to the new one. The expected
//! scalar values are externally deducible from §7.5 count semantics.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Precision, Subscription, SurfaceBinding, SurfaceHost, SurfaceOutcome,
    SurfaceRouter, SurfaceRouterBuilder, SurfaceWatch, Value, ViewBinding, ViewDelta, ViewResult,
    VirtualClock,
};
use liasse_value::Integer;
use support::{address, call, store, text, NOW};

const COUNT_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.countcases@1.0.0"
  "$model": {
    "items": { "$key": "name", "name": "text" }
    "total": { "$view": "count(.items)" }
    "$mut": {
      "add": ".items + { name: @name }"
      "drop": ".items - @name"
    }
    "$public": {
      "count": { "$view": ".total" }
      "items": { "$mut": { "add": ".add", "drop": ".drop" } }
    }
  }
}"#;

fn count_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = Engine::load(store("countcases"), COUNT_APP, &mut clock).expect("count app loads");
    let router = count_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn count_router(model: &liasse_model::Model) -> SurfaceRouter {
    let count = SurfaceBinding::new().with_view(ViewBinding::new("total"));
    let items = SurfaceBinding::new()
        .with_call("add", CallBinding::root("add", ["name".to_owned()]))
        .with_call("drop", CallBinding::root("drop", ["name".to_owned()]));
    SurfaceRouterBuilder::new()
        .public_surface("count", count)
        .public_surface("items", items)
        .build(model)
        .expect("router validates against the count model")
}

fn total(host: &SurfaceHost<MemoryStore>) -> ViewResult {
    host.engine().view_at_head("total").expect("view evaluates").expect("total declared")
}

fn add(host: &mut SurfaceHost<MemoryStore>, name: &str) {
    let outcome = host.call("c1", &call("public.items.add", [("name", text(name))])).expect("add");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "add commits: {outcome:?}");
}

fn count(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

/// A faithful §12.2 client that follows the protocol using ONLY what the delta
/// primitive carries, then reports the scalar it renders. A scalar view's delta is
/// the value form: `Scalar(Some(value))` sets the client's scalar to `value` (at
/// first observation or on a change); `Scalar(None)` is the frontier-only no-op,
/// leaving the prior value. A row-shaped delta over a scalar subscription would be
/// a shape error the primitive never produces (§7.5, §12.2).
fn client_scalar(prior: Option<&Value>, delta: &ViewDelta) -> Option<Value> {
    match delta {
        ViewDelta::Scalar(Some(value)) => Some(value.clone()),
        ViewDelta::Scalar(None) => prior.cloned(),
        ViewDelta::Init(_) | ViewDelta::Patch(_) => {
            panic!("a scalar `count` view yields a scalar delta, not a row delta")
        }
    }
}

#[test]
fn scalar_subscription_is_a_live_reachable_path() {
    // Reachability probe (expected to PASS): a scalar `count` view IS a subscribable
    // surface view. The live subscription opens over it and delivers count 1; a
    // same-connection commit runs the §12.3 barrier and the snapshot read reflects
    // count 2. This establishes that the scalar view is a genuine §12.2 live path —
    // the delta primitive below is the missing link, not the subscription itself.
    let mut host = count_host();
    host.connect("c1").unwrap();
    add(&mut host, "a");

    let init = match host.watch("c1", &SurfaceWatch::new(address("public.count"), "w1")).expect("watch") {
        Subscription::Init(result) => result,
        other => panic!("expected a scalar init, got {other:?}"),
    };
    assert_eq!(init.scalar(), Some(&count(1)), "the live subscription delivers count 1 at open");

    add(&mut host, "b"); // same-connection commit -> §12.3 barrier advances the watch
    let snapshot = host.read_view("c1", "w1").expect("cached result present");
    assert_eq!(snapshot.scalar(), Some(&count(2)), "the barrier's snapshot read reflects the new count 2");
}

#[test]
fn scalar_view_init_delivers_the_initial_count() {
    // §7.5: with one item, count(.items) == 1. §12.2: a subscription "begins with
    // a complete result" — the client must be able to render that initial value.
    let mut host = count_host();
    host.connect("c1").unwrap();
    add(&mut host, "a");
    let next = total(&host);
    assert_eq!(next.scalar(), Some(&count(1)), "count(.items) with one item is 1 (§7.5)");

    let delta = ViewDelta::between(None, &next);
    let rendered = client_scalar(None, &delta);
    assert_eq!(
        rendered,
        Some(count(1)),
        "§12.2: the initial complete result of a scalar `count` subscription must convey the \
         count 1; ViewDelta::between produced {delta:?}",
    );
}

#[test]
fn scalar_view_patch_conveys_the_changed_count() {
    // §7.5: count goes 1 -> 2 on a second insert. §12.2: after applying the patch
    // "the client result MUST equal the authorized declared view at the new
    // frontier", i.e. the client must reach 2.
    let mut host = count_host();
    host.connect("c1").unwrap();
    add(&mut host, "a");
    let prev = total(&host);
    assert_eq!(prev.scalar(), Some(&count(1)), "count is 1 after the first insert (§7.5)");

    add(&mut host, "b");
    let next = total(&host);
    assert_eq!(next.scalar(), Some(&count(2)), "count is 2 after the second insert (§7.5)");

    let delta = ViewDelta::between(Some(&prev), &next);
    let rendered = client_scalar(prev.scalar(), &delta);
    assert_eq!(
        rendered,
        Some(count(2)),
        "§12.2: after applying the delta the client result MUST equal the authorized declared \
         view (count 2) at the new frontier; ViewDelta::between produced {delta:?}",
    );
}
