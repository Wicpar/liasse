#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 bounded-window gap under a MIXED-direction multi-key `$sort` (§7.3).
//!
//! SPEC.md §12.2: a departed anchor freezes "its last complete **sort tuple plus
//! occurrence identity** as an immutable ordered gap. That coordinate determines
//! the window until the occurrence reappears." The gap is a fixed position in the
//! view's *total sort order* — and §7.3 lets that order combine descending and
//! ascending keys (`"$sort": ["-created_at", "id"]`), with occurrence identity as
//! the §8/Annex B.5 final tiebreak.
//!
//! This locks the whole class down: the `open` view sorts by `[-rank, id]` — rank
//! DESCENDING, then id ASCENDING. `liasse-runtime`'s `order_rows` and a bounded
//! window's gap partition now share one comparator (`SortOrder::compare`), so the
//! window must resume at the first row at/after the departed anchor's
//! `(rank, id)` coordinate in exactly that mixed order — not an all-ascending
//! approximation of it.
//!
//! Ranks: a=1, b=c=d=2, e=3. The order `[-rank, id]` is fixed and externally
//! deducible: e(3) > {b,c,d}(2, id asc) > a(1), i.e. [e, b, c, d, a].

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Precision, RowId, Subscription, SurfaceBinding, SurfaceHost, SurfaceRouter,
    SurfaceRouterBuilder, SurfaceWatch, ViewBinding, ViewRow, VirtualClock, Window,
};
use support::{call, store, text, NOW};

/// Items keyed by a text id, each with an int `rank`, and an `open` view that
/// drops completed rows and sorts them by `[-rank, id]` — rank DESCENDING then id
/// ASCENDING, the mixed-direction multi-key order §7.3 admits.
const ITEMS_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.mixeditems@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "rank": "int = 0", "done": "bool = false" }
    "index": { "$view": ".items { id, rank, done, $sort: [id] }" }
    "open": { "$view": ".items[:i | !i.done] { id, rank, $sort: [-rank, id] }" }
    "$mut": {
      "add": ".items + { id: @id, rank: @rank }"
      "complete": ".items[@id].done = true"
    }
    "$public": {
      "items": {
        "$view": ".index"
        "$mut": { "add": ".add", "complete": ".complete" }
      }
      "open": { "$view": ".open" }
    }
  }
  "$data": {
    "items": {
      "a": { "rank": 1 }
      "b": { "rank": 2 }
      "c": { "rank": 2 }
      "d": { "rank": 2 }
      "e": { "rank": 3 }
    }
  }
}"#;

fn items_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = Engine::load(store("mixeditems"), ITEMS_APP, &mut clock).expect("items app loads");
    let router = items_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn items_router(model: &liasse_model::Model) -> SurfaceRouter {
    let items = SurfaceBinding::new()
        .with_view(ViewBinding::new("index"))
        .with_call("add", CallBinding::root("add", ["id".to_owned(), "rank".to_owned()]))
        .with_call("complete", CallBinding::root("complete", ["id".to_owned()]));
    let open = SurfaceBinding::new().with_view(ViewBinding::new("open"));
    SurfaceRouterBuilder::new()
        .public_surface("items", items)
        .public_surface("open", open)
        .build(model)
        .expect("router validates against the items model")
}

/// The `id` text of each windowed row, in order.
fn ids(rows: &[ViewRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row.field("id") {
            Some(liasse_surface::Value::Text(t)) => t.as_str().to_owned(),
            other => panic!("unexpected id cell {other:?}"),
        })
        .collect()
}

/// The window rows currently tracked by subscription `id` on `conn`.
fn window(host: &SurfaceHost<MemoryStore>, conn: &str, id: &str) -> Vec<String> {
    ids(host.read_window(conn, id).expect("windowed subscription present"))
}

/// The stable occurrence identity of the row exposing `id` in the `open` view.
fn occurrence(host: &SurfaceHost<MemoryStore>, id: &str) -> RowId {
    let view = host.engine().view_at_head("open").expect("view").expect("declared");
    view.rows()
        .iter()
        .find(|row| row.field("id") == Some(&text(id)))
        .map(|row| row.id().clone())
        .expect("row present")
}

#[test]
fn absent_anchor_gap_respects_a_mixed_direction_multi_key_sort() {
    let mut host = items_host();
    host.connect("c1");

    // Order `[-rank, id]` over the `open` view: [e, b, c, d, a]. Open a size-2
    // window anchored on "c" — the anchor becomes the first row (§12.2), and the
    // next row in this mixed order is "d" (same rank 2, id "d" > "c"): [c, d].
    let anchor = occurrence(&host, "c");
    let watch = SurfaceWatch::new(support::address("public.open"), "w1")
        .with_window(Window::anchored(2, anchor));
    match host.watch("c1", &watch).expect("watch") {
        Subscription::Window(rows) => {
            assert_eq!(ids(&rows), ["c", "d"], "anchor becomes the first row in the mixed order")
        }
        other => panic!("expected a windowed init, got {other:?}"),
    }

    // Complete "c": the anchor leaves the `open` view, now [e, b, d, a]. §12.2
    // freezes the pair (sort tuple (rank=2, id="c"), occurrence=c). In the mixed
    // order `[-rank, id]` the coordinate sits in the rank-2 group between "b" and
    // "d" (id asc), so the first two rows AT OR AFTER it are [d, a]. "e" and "b"
    // sort BEFORE the departed anchor and can never be the window's first row;
    // "a" (rank 1) sorts after the whole rank-2 group because rank is descending.
    host.call("c1", &call("public.items.complete", [("id", text("c"))]))
        .expect("complete")
        .commit()
        .expect("commit");

    assert_eq!(
        window(&host, "c1", "w1"),
        ["d", "a"],
        "a mixed-direction gap must resume at the first row at/after the anchor's (rank, id) coordinate",
    );
}
