#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 live-view coherence for a BOUNDED-WINDOW subscription — RED TEAM.
//!
//! SPEC.md §12.2 fixes ONE live-view contract for every subscription, windowed or
//! not:
//!
//! ```text
//! init(frontier, rows)
//! patch(frontier, operations)
//! ```
//! "`$at` and `$to` are zero-based positions in the current result. ... After
//! applying every operation, the client result MUST equal the authorized declared
//! view at the new frontier. A frontier-only patch has an empty operation
//! sequence."
//!
//! A bounded window is not a second, snapshot-only channel: §12.2 introduces it as
//! the mechanism that keeps *large views incremental*. So a windowed
//! subscription's client result is its WINDOW, and the ordered patch the runtime
//! emits at each frontier MUST carry the client's prior window to the new
//! authorized window — same occurrences, same order, same bounded size — with
//! every `$at`/`$to` a position in the CURRENT (windowed) client result.
//!
//! [`Watch`] is the shipped §12.2 view-tracking primitive: `init` delivers the
//! initial window and `advance` returns the patch a client applies to reach the
//! next frontier ([`Watch::window_rows`] is that same client-visible window,
//! recomputed). These probes open a `first(2)` window, commit one mutation, and
//! assert the §12.2 MUST: applying `advance`'s delta to the PRIOR window yields the
//! new authorized window. The expected windows are externally deducible from the
//! view's `$sort: [id]` and `$size: 2` — never the algorithm's own output.
//!
//! Both currently FAIL: `Watch::advance` computes `ViewDelta::between` over the
//! FULL view (`self.last`), not over the window, so the emitted patch carries
//! full-view positions and full-view membership — it neither evicts the row the
//! window pushed past its bound nor keeps its `$at` inside the client-visible
//! window.

mod support;

use std::collections::BTreeMap;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, PatchOp, Precision, RowId, SurfaceBinding, SurfaceHost, SurfaceRouter,
    SurfaceRouterBuilder, Value, ViewBinding, ViewDelta, ViewResult, ViewRow, VirtualClock, Watch,
    WatchAuthz, Window,
};
use support::{call, store, text, NOW};

/// A tiny keyed, `id`-sorted collection with an `add` mutation — the shape §12.2's
/// window examples need, kept minimal and seedless so each probe builds an exact,
/// externally known row set.
const APP: &str = r#"{
  "$liasse": 1
  "$app": "example.windelta@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text" }
    "index": { "$view": ".items { id, $sort: [id] }" }
    "$mut": { "add": ".items + { id: @id }" }
    "$public": {
      "items": {
        "$view": ".index"
        "$mut": { "add": ".add" }
      }
    }
  }
}"#;

fn windelta_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = Engine::load(store("windelta"), APP, &mut clock).expect("app loads");
    let router = windelta_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn windelta_router(model: &liasse_model::Model) -> SurfaceRouter {
    let items = SurfaceBinding::new()
        .with_view(ViewBinding::new("index"))
        .with_call("add", CallBinding::root("add", ["id".to_owned()]));
    SurfaceRouterBuilder::new()
        .public_surface("items", items)
        .build(model)
        .expect("router validates against the model")
}

/// The full authorized `index` view at head — the runtime input a subscription is
/// recomputed from at every frontier (§12.2).
fn index(host: &SurfaceHost<MemoryStore>) -> ViewResult {
    host.engine().view_at_head("index").expect("view evaluates").expect("index declared")
}

fn add(host: &mut SurfaceHost<MemoryStore>, id: &str) {
    host.call("c1", &call("public.items.add", [("id", text(id))]))
        .expect("add dispatches")
        .commit()
        .expect("add commits");
}

/// The client-visible content of a windowed result: each occurrence identity and
/// its exposed fields, in order. This is exactly what §12.2 requires the client
/// window to equal after applying the patch.
fn visible(rows: &[ViewRow]) -> Vec<(RowId, BTreeMap<String, Value>)> {
    rows.iter()
        .map(|row| (row.id().clone(), row.fields().map(|(k, v)| (k.clone(), v.clone())).collect()))
        .collect()
}

/// A faithful §12.2 client applying the ordered patch to its prior CLIENT-VISIBLE
/// result (the window), one op at a time, each `$at`/`$to` read in the current
/// windowed result. A position outside the client-visible window is a malformed
/// patch for that client (§12.2: positions are "in the current result").
fn apply_patch(prior: &[ViewRow], delta: &ViewDelta) -> Vec<ViewRow> {
    match delta {
        ViewDelta::Init(rows) => rows.clone(),
        ViewDelta::Patch(ops) => {
            let mut rows = prior.to_vec();
            for op in ops {
                match op {
                    PatchOp::Remove { id } => {
                        let at = position(&rows, id, "remove");
                        rows.remove(at);
                    }
                    PatchOp::Update { row } => {
                        let at = position(&rows, row.id(), "update");
                        rows[at] = row.clone();
                    }
                    PatchOp::Move { id, to } => {
                        let at = position(&rows, id, "move");
                        let row = rows.remove(at);
                        assert!(
                            *to <= rows.len(),
                            "§12.2: `move $to={to}` is outside the current windowed client result \
                             (len {}); the emitted patch is not a windowed patch",
                            rows.len(),
                        );
                        rows.insert(*to, row);
                    }
                    PatchOp::Insert { at, row } => {
                        assert!(
                            *at <= rows.len(),
                            "§12.2: `insert $at={at}` is outside the current windowed client \
                             result (len {}); the emitted patch carries a full-view position, not \
                             a windowed one",
                            rows.len(),
                        );
                        rows.insert(*at, row.clone());
                    }
                    PatchOp::Rekey { .. } => unreachable!("between renders a key change as remove+insert"),
                }
            }
            rows
        }
        ViewDelta::Scalar(_) => unreachable!("a row view never yields a scalar delta"),
    }
}

fn position(rows: &[ViewRow], id: &RowId, op: &str) -> usize {
    rows.iter().position(|row| row.id() == id).unwrap_or_else(|| {
        panic!("§12.2: `{op}` targets an occurrence absent from the current windowed client result")
    })
}

/// The `id` text of each row, in order — for readable failure output.
fn ids(rows: &[ViewRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row.field("id") {
            Some(Value::Text(t)) => t.as_str().to_owned(),
            other => panic!("unexpected id cell {other:?}"),
        })
        .collect()
}

/// Open a `first(2)` window over the current `index`, returning the watch and its
/// initial client-visible window (the prior state the next patch applies to).
fn open_first2(host: &SurfaceHost<MemoryStore>) -> (Watch, Vec<ViewRow>) {
    let seq = host.engine().head();
    let mut watch = Watch::windowed("index", WatchAuthz::public(), seq, Window::first(2));
    watch.init(index(host), seq).expect("the first(2) window opens");
    let prior = watch.window_rows().expect("windowed subscription has rows").to_vec();
    (watch, prior)
}

#[test]
fn front_insert_keeps_the_window_bounded_and_evicts_the_pushed_row() {
    // Full view [b, c, d]; the client's first(2) window is [b, c].
    let mut host = windelta_host();
    host.connect("c1");
    add(&mut host, "b");
    add(&mut host, "c");
    add(&mut host, "d");
    let (mut watch, prior) = open_first2(&host);
    assert_eq!(ids(&prior), ["b", "c"], "the first(2) window opens on [b, c]");

    // Insert "a" at the front. Full view becomes [a, b, c, d]; the authorized
    // first(2) window is now [a, b] — "c" is pushed past the size-2 bound and MUST
    // leave the client's window. This expectation is fixed by `$sort: [id]` and
    // `$size: 2`, not by any delta the runtime produced.
    add(&mut host, "a");
    let seq = host.engine().head();
    let delta = watch.advance(index(&host), seq);
    let authorized = watch.window_rows().expect("windowed rows").to_vec();
    assert_eq!(ids(&authorized), ["a", "b"], "the recomputed authorized window is [a, b]");

    let client = apply_patch(&prior, &delta);
    assert_eq!(
        visible(&client),
        visible(&authorized),
        "§12.2: after applying the emitted patch the client's WINDOW must equal the authorized \
         window [a, b] at the new frontier — a bounded window kept incremental. The full-view \
         delta {delta:?} inserts \"a\" but never evicts \"c\", leaving the client window \
         {:?} instead of {:?}.",
        ids(&client),
        ids(&authorized),
    );
}

#[test]
fn tail_insert_that_leaves_the_window_unchanged_is_a_frontier_only_patch() {
    // Full view [a, b, c]; the client's first(2) window is [a, b].
    let mut host = windelta_host();
    host.connect("c1");
    add(&mut host, "a");
    add(&mut host, "b");
    add(&mut host, "c");
    let (mut watch, prior) = open_first2(&host);
    assert_eq!(ids(&prior), ["a", "b"], "the first(2) window opens on [a, b]");

    // Insert "d" at the tail. Full view becomes [a, b, c, d]; the authorized
    // first(2) window is STILL [a, b] — the commit does not touch the window. §12.2:
    // an unchanged client result is a frontier-only patch (empty op sequence).
    add(&mut host, "d");
    let seq = host.engine().head();
    let delta = watch.advance(index(&host), seq);
    let authorized = watch.window_rows().expect("windowed rows").to_vec();
    assert_eq!(ids(&authorized), ["a", "b"], "the recomputed authorized window is unchanged [a, b]");

    // Applying the emitted patch to the prior window must reproduce [a, b]. The
    // full-view delta is `insert { at: 3 }` — a position outside the 2-row window —
    // so a faithful windowed client cannot even place it.
    let client = apply_patch(&prior, &delta);
    assert_eq!(
        visible(&client),
        visible(&authorized),
        "§12.2: a commit that does not change the client's window must yield a frontier-only \
         (empty) windowed patch; the runtime emitted {delta:?}",
    );
}
