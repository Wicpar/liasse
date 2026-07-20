#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM (WAVE 4) — §12.1 closed argument object is NOT enforced on the REAL
//! `SurfaceHost::watch`/`resume`; the wave-3 F2 fix landed only in the testkit
//! ADAPTER (`crates/liasse-testkit/src/adapter/runtime.rs`), so a real wire client
//! bypasses the rule entirely.
//!
//! §12.1 (SPEC.md, verbatim): "An argument object presented to a `call` or `view`
//! request is closed: it MUST contain only names that are declared parameters of
//! the targeted mutation or view. A member whose name is not a declared parameter
//! — including any reserved `$`-prefixed name — makes the request malformed; the
//! runtime rejects it during parameter parsing (step 3), before admission, with no
//! partial effect. There is no width subtyping over external argument objects, and
//! an undeclared member is never silently dropped."
//!
//! The wave-3 commit 879b0bd claims to fix F2 "§12.1 closed-arg on view/watch/
//! resume", but its own body shows the enforcement was added to
//! `crates/liasse-testkit/src/adapter/runtime.rs` — the testkit adapter — NOT to
//! the real surface. `SurfaceHost::watch` (host/call.rs:487) and `resume`
//! (host/call.rs:518) route the client args through `view_query` (host/call.rs:776,
//! which binds EVERY arg via `ViewQuery::param` with no filtering) and then through
//! `Engine::view_with` → `bind_params` (engine.rs:1613, which binds every supplied
//! param and fills declared-but-omitted ones with defaults, with no rejection of an
//! undeclared or `$`-prefixed member). No layer on the real surface path closes the
//! argument object, so an undeclared / reserved-name view argument is silently
//! dropped and the subscription opens with a served result.
//!
//! EXPECTED (§12.1): the request is MALFORMED and is refused before any result is
//! served — no `Init`/`Window`. (The `Subscription` type has no `rejected` arm, so
//! a §12.1 refusal on this path can only surface as `Denied`, exactly as the
//! commit's own note describes the intended fix.)
//! ACTUAL: `SurfaceHost::watch` returns `Subscription::Init(result)` — the
//! undeclared member was silently dropped and the filtered view was served.
//!
//! Root cause: `crates/liasse-surface/src/host/call.rs::view_query` (L776) +
//! `crates/liasse-runtime/src/engine.rs::bind_params` (L1613) — neither closes the
//! argument object against the view's declared `$params`. The closed-shape check
//! exists only in the testkit adapter, so the corpus (which runs through the
//! adapter) is green while the shipped surface API is open.
//!
//! Severity: HIGH — a real wire client using the public `SurfaceHost::watch`/
//! `resume` API bypasses a MUST rule the spec pins for a security-relevant reason
//! (§12.3 dedup identity rests on the closed argument set: "No ignored-but-present
//! member can silently vary between two submissions the runtime would otherwise
//! treat as one operation"). The corpus cannot catch this because it never reaches
//! the real surface.

use std::collections::BTreeMap;

use liasse_ident::InstanceId;
use liasse_store::MemoryStore;
use liasse_surface::{
    Engine, Precision, Subscription, SurfaceAddress, SurfaceBinding, SurfaceHost, SurfaceResume,
    SurfaceRouterBuilder, SurfaceWatch, Value, ViewBinding, ViewResult, VirtualClock,
};
use liasse_value::Text;

/// A parameterized public surface view declaring exactly ONE parameter, `owner`.
/// Per §12.1 an argument object may carry ONLY `owner`; any other member — plain or
/// `$`-prefixed — makes the request malformed.
const PARAM_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.paramview.w4@1.0.0"
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

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn address(target: &str) -> SurfaceAddress {
    SurfaceAddress::parse(target).expect("address parses")
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

fn probe_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(1_700_000_000_000_000, Precision::Micros);
    let store = MemoryStore::new(InstanceId::new("paramview-w4"));
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

/// A `{ owner: "alice", <extra>: <value> }` argument map: the declared `owner`
/// plus one undeclared member `name`.
fn args_with_extra(extra_name: &str, extra_value: Value) -> BTreeMap<String, Value> {
    let mut args = BTreeMap::new();
    args.insert("owner".to_owned(), text("alice"));
    args.insert(extra_name.to_owned(), extra_value);
    args
}

/// Whether a subscription outcome SERVED a result (opened) rather than refusing.
fn served(sub: &Subscription) -> bool {
    matches!(sub, Subscription::Init(_) | Subscription::Window(_))
}

// ── PASSING CONTROL: only the declared `owner` arg → the request is well-formed
//    and the alice-filtered view is served. This isolates the defect: the closed
//    shape rule is the ONLY thing that should differ between this and the findings.
#[test]
fn control_only_declared_arg_opens() {
    let mut host = probe_host();
    host.connect("c1").unwrap();
    let mut args = BTreeMap::new();
    args.insert("owner".to_owned(), text("alice"));
    let watch = SurfaceWatch::new(address("public.owned"), "w1").with_args(args);
    match host.watch("c1", &watch).expect("watch") {
        Subscription::Init(result) => assert_eq!(ids(&result), ["t1"], "declared arg filters to alice"),
        other => panic!("a well-formed declared arg must open; got {other:?}"),
    }
}

// ── FINDING 1: an UNDECLARED plain member (`bogus`) on a `watch` argument object.
//    §12.1: "MUST contain only names that are declared parameters ... an undeclared
//    member is never silently dropped ... makes the request malformed." The real
//    surface silently drops `bogus` and serves the alice-filtered view.
#[test]
fn watch_rejects_undeclared_view_argument() {
    let mut host = probe_host();
    host.connect("c1").unwrap();
    let watch = SurfaceWatch::new(address("public.owned"), "w1")
        .with_args(args_with_extra("bogus", text("x")));
    let sub = host.watch("c1", &watch).expect("watch");
    assert!(
        !served(&sub),
        "§12.1: an argument object carrying the undeclared member `bogus` is MALFORMED \
         and MUST be refused before any result is served; the real `SurfaceHost::watch` \
         silently dropped it and served a subscription instead: {sub:?}",
    );
}

// ── FINDING 2: a RESERVED `$`-prefixed member (`$actor`) on a `watch` argument
//    object. §12.1 names this case explicitly: "including any reserved `$`-prefixed
//    name — makes the request malformed." The real surface accepts it.
#[test]
fn watch_rejects_reserved_dollar_prefixed_view_argument() {
    let mut host = probe_host();
    host.connect("c1").unwrap();
    let watch = SurfaceWatch::new(address("public.owned"), "w1")
        .with_args(args_with_extra("$actor", text("smuggled")));
    let sub = host.watch("c1", &watch).expect("watch");
    assert!(
        !served(&sub),
        "§12.1: a reserved `$`-prefixed argument member (`$actor`) is MALFORMED and MUST \
         be refused; the real `SurfaceHost::watch` accepted it and opened a subscription: {sub:?}",
    );
}

// ── FINDING 3: the same closed-shape rule on the `resume` path. §12.1 applies to
//    every `view` request; a resume reconstructs the `view` operation (§12.2), so an
//    undeclared member on resume is equally malformed. The real `resume` accepts it.
#[test]
fn resume_rejects_undeclared_view_argument() {
    let mut host = probe_host();
    host.connect("c1").unwrap();
    // Open a well-formed subscription first to obtain a resume frontier.
    let mut ok_args = BTreeMap::new();
    ok_args.insert("owner".to_owned(), text("alice"));
    let watch = SurfaceWatch::new(address("public.owned"), "w1").with_args(ok_args);
    assert!(served(&host.watch("c1", &watch).expect("watch")), "the opening watch serves");
    let from = host.frontier("c1").expect("connection open");
    host.disconnect("c1");

    host.connect("c2").unwrap();
    let resume = SurfaceResume::new(address("public.owned"), "w2", from)
        .with_args(args_with_extra("bogus", text("x")));
    let sub = host.resume("c2", &resume).expect("resume");
    assert!(
        !served(&sub),
        "§12.1: a `resume` reconstructing the `view` operation with the undeclared member \
         `bogus` is MALFORMED and MUST be refused; the real `SurfaceHost::resume` served it: {sub:?}",
    );
}
