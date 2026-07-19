#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing, clippy::iter_count)]
//! §12.2 live-view coherence — RED TEAM against commit `ac07db5`.
//!
//! DEFECT: a bounded-window subscription opened over a SCALAR / AGGREGATE `$view`
//! (§7.5) silently DROPS the view's value. `ac07db5` made a windowed subscription
//! diff over WINDOW SLICES (`ViewDelta::between_rows(prior_window, refreshed)`), on
//! the stated premise that "a window is always a row stream". But the runtime lets
//! a window open over a scalar view — neither `Watch::windowed` nor the surface's
//! `open_subscription` reject it — and a scalar `ViewResult` has NO rows, so the
//! window slice is always empty. `init` therefore emits `Init([])` and `advance`
//! emits `Patch([])`, and the scalar value never reaches the subscriber.
//!
//! SPEC.md §12.2: "After applying every operation, the client result MUST equal the
//! authorized declared view at the new frontier." SPEC.md §7.5/§12.2: a scalar or
//! aggregate `$view` delivers its VALUE (`ViewDelta::Scalar`), rendered as the JSON
//! scalar. Here the authorized declared `count` view is the scalar `2`, and after a
//! commit it is `3` — both externally deducible from `size(.items)`. The runtime
//! ACCEPTS the windowed subscription (it does not reject it), so §12.2 binds: the
//! subscriber must be able to observe those values. It observes neither.
//!
//! This is a REGRESSION `ac07db5` introduced: the prior code computed
//! `ViewDelta::between(self.last, result)` even for a windowed subscription, so a
//! window over a scalar view still emitted `Scalar(Some(2))` / `Scalar(Some(3))` —
//! the value reached the client. The fix's switch to empty window-slice diffs lost
//! it. Either resolution keeps the value coherent: reject a window over a scalar
//! view at open (as an absent anchor is rejected), or carry the scalar delta for a
//! scalar view regardless of the window. Silently reporting an empty window as the
//! authorized view is neither.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Precision, Subscription, SurfaceBinding, SurfaceHost, SurfaceRouter,
    SurfaceRouterBuilder, SurfaceWatch, Value, ViewBinding, ViewDelta, ViewResult, VirtualClock,
    Watch, WatchAuthz, Window,
};
use liasse_value::Integer;
use support::{call, store, text, NOW};

/// A keyed collection plus an AGGREGATE view `count = size(.items)` (§7.5): its
/// result is a single scalar value, not a row stream.
const APP: &str = r#"{
  "$liasse": 1
  "$app": "example.winscalar@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text" }
    "index": { "$view": ".items { id, $sort: [id] }" }
    "count": { "$view": "= size(.items)" }
    "$mut": { "add": ".items + { id: @id }" }
    "$public": {
      "items": { "$view": ".index", "$mut": { "add": ".add" } }
      "count": { "$view": ".count" }
    }
  }
}"#;

fn scalar_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = Engine::load(store("winscalar"), APP, &mut clock).expect("app loads");
    let router = router(engine.model());
    let mut host = SurfaceHost::new(engine, router, clock);
    host.connect("c1").unwrap();
    host
}

fn router(model: &liasse_model::Model) -> SurfaceRouter {
    let items = SurfaceBinding::new()
        .with_view(ViewBinding::new("index"))
        .with_call("add", CallBinding::root("add", ["id".to_owned()]));
    let count = SurfaceBinding::new().with_view(ViewBinding::new("count"));
    SurfaceRouterBuilder::new()
        .public_surface("items", items)
        .public_surface("count", count)
        .build(model)
        .expect("router validates")
}

fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}

fn count_view(host: &SurfaceHost<MemoryStore>) -> ViewResult {
    host.engine().view_at_head("count").expect("view evaluates").expect("count declared")
}

fn add(host: &mut SurfaceHost<MemoryStore>, id: &str) {
    host.call("c1", &call("public.items.add", [("id", text(id))]))
        .expect("dispatch")
        .commit()
        .expect("commit");
}

/// The scalar value a §12.2 delta conveys to the subscriber, if any. A row-stream
/// delta (`Init`/`Patch`) conveys NO scalar — a faithful client tracking it holds a
/// row list, never a scalar — so this is `None` for those.
fn conveyed_scalar(delta: &ViewDelta) -> Option<Value> {
    match delta {
        ViewDelta::Scalar(value) => value.clone(),
        ViewDelta::Init(_) | ViewDelta::Patch(_) => None,
    }
}

#[test]
fn windowed_subscription_over_a_scalar_view_delivers_the_authorized_value() {
    let mut host = scalar_host();
    add(&mut host, "a");
    add(&mut host, "b");

    // The authorized declared `count` view is the scalar 2 (externally deducible:
    // size(.items) over {a, b}). An UNWINDOWED subscription delivers it as the §7.5
    // scalar delta — the reference for what §12.2 must convey for this view.
    let seq = host.engine().head().unwrap();
    let mut plain = Watch::open("count", WatchAuthz::public(), seq);
    let plain_init = plain.init(count_view(&host), seq).expect("plain init");
    assert_eq!(
        conveyed_scalar(&plain_init),
        Some(int(2)),
        "reference: an unwindowed subscription over the scalar view conveys 2 ({plain_init:?})",
    );

    // A WINDOWED subscription over the SAME scalar view. The runtime ACCEPTS it
    // (init returns Ok — no rejection), so §12.2 binds: after applying the init the
    // client MUST hold the authorized declared view, the scalar 2.
    let mut windowed = Watch::windowed("count", WatchAuthz::public(), seq, Window::first(4));
    let win_init = windowed.init(count_view(&host), seq).expect("windowed init is accepted, not rejected");
    assert_eq!(
        conveyed_scalar(&win_init),
        Some(int(2)),
        "§12.2/§7.5: a windowed subscription the runtime ACCEPTED over a scalar view must still \
         deliver the authorized value 2; `ac07db5` diffs an empty window slice and drops it — \
         the subscriber receives {win_init:?}, which conveys no value",
    );

    // A commit changes the authorized scalar view from 2 to 3. §12.3: the runtime
    // advances every still-authorized active subscription through the commit; §12.2:
    // after applying the emitted ops the client result MUST equal the new authorized
    // view (the scalar 3). The windowed advance is a frontier-only empty `Patch([])`,
    // so the subscriber never observes the change.
    add(&mut host, "c");
    let seq = host.engine().head().unwrap();
    let win_advance = windowed.advance(count_view(&host), seq);
    assert_eq!(
        conveyed_scalar(&win_advance),
        Some(int(3)),
        "§12.2: a commit that changed the authorized scalar view from 2 to 3 must convey 3 to the \
         accepted windowed subscription; it emitted {win_advance:?}, a frontier-only no-op that \
         never delivers the new value",
    );
}

#[test]
fn surface_watch_over_a_scalar_view_is_not_silently_an_empty_window() {
    // The defect is reachable through the ordinary surface API, not just the raw
    // primitive: `host.watch(..).with_window(..)` over a scalar-view surface is
    // ACCEPTED and returns an empty window, so the aggregate value is never
    // delivered — and a commit that changes it leaves the window empty forever.
    let mut host = scalar_host();
    add(&mut host, "a");
    add(&mut host, "b");

    let watch = SurfaceWatch::new(support::address("public.count"), "w1").with_window(Window::first(4));
    let opened = host.watch("c1", &watch).expect("watch dispatches");

    // §12.2/§7.5: the runtime must not present a scalar/aggregate view as an empty
    // bounded window. It must either reject the window (like an absent anchor) or
    // deliver the scalar value 2 — never accept-and-return-empty.
    match opened {
        Subscription::Window(rows) => panic!(
            "§12.2: a windowed subscription over the scalar `count` view (authorized value 2) was \
             accepted as an empty bounded window {:?}, silently dropping the aggregate value",
            rows.iter().count(),
        ),
        Subscription::Failed(_) | Subscription::Denied(_) => {
            // A clean rejection is an acceptable resolution — the subscriber is told
            // a scalar view cannot be windowed, rather than handed a lossy empty view.
        }
        other => panic!("unexpected subscription outcome {other:?}"),
    }
}
