#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED (§12.2 bounded window "immutable ordered gap"):
//!
//! SPEC.md §12.2: "If it later leaves the view, the subscription retains its last
//! complete **sort tuple** plus occurrence identity as an immutable ordered gap.
//! That coordinate determines the window until the occurrence reappears or the
//! subscription is reopened."
//!
//! The retained gap coordinate is the anchor's own *sort tuple* — a fixed
//! position in the total sort order. So while the anchor is absent, a bounded
//! window must begin at the first current row whose sort tuple is **at or after**
//! that coordinate. A row that sorts *before* the coordinate can never be the
//! window's first row.
//!
//! `crates/liasse-surface/src/window.rs` does not retain a sort tuple. It freezes
//! the anchor's left/right *neighbor row identities* and, while the anchor is
//! absent, starts the window "just past the frozen left neighbor" (`gap_start`).
//! When a new row is inserted between the frozen left neighbor and the true
//! coordinate, that new row is "just past the left neighbor" yet sorts *before*
//! the coordinate, so the implementation wrongly places it at the window start —
//! contradicting both §12.2 and `window.rs`'s own documented "first rows at or
//! after the gap coordinate" contract.
//!
//! Items are keyed and sorted by a text id, so the ordering is fixed and
//! externally deducible: `a < b < bb < c < d < e`.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Precision, RowId, Subscription, SurfaceBinding, SurfaceHost, SurfaceRouter,
    SurfaceRouterBuilder, SurfaceWatch, ViewBinding, ViewRow, VirtualClock, Window,
};
use support::{call, store, text, NOW};

/// Items keyed and sorted by a text id, with a `done` flag and an `open` view that
/// drops completed rows — the shape §12.2's gap example needs.
const ITEMS_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.gapitems@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "done": "bool = false" }
    "index": { "$view": ".items { id, done, $sort: [id] }" }
    "open": { "$view": ".items[:i | !i.done] { id, $sort: [id] }" }
    "$mut": {
      "add": ".items + { id: @id }"
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
    "items": { "a": {}, "b": {}, "c": {}, "d": {}, "e": {} }
  }
}"#;

fn items_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = Engine::load(store("gapitems"), ITEMS_APP, &mut clock).expect("items app loads");
    let router = items_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn items_router(model: &liasse_model::Model) -> SurfaceRouter {
    let items = SurfaceBinding::new()
        .with_view(ViewBinding::new("index"))
        .with_call("add", CallBinding::root("add", ["id".to_owned()]))
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
fn gap_coordinate_is_the_anchor_sort_tuple_not_a_row_before_it() {
    let mut host = items_host();
    host.connect("c1").unwrap();

    // Open a size-2 window anchored on "c" over the `open` view [a, b, c, d, e].
    // "A concrete anchor normally becomes the first row" -> [c, d].
    let anchor = occurrence(&host, "c");
    let watch = SurfaceWatch::new(support::address("public.open"), "w1").with_window(Window::anchored(2, anchor));
    match host.watch("c1", &watch).expect("watch") {
        Subscription::Window(rows) => assert_eq!(ids(&rows), ["c", "d"], "anchor becomes the first row"),
        other => panic!("expected a windowed init, got {other:?}"),
    }

    // Complete "c": the anchor leaves the `open` view. The frozen sort-tuple
    // coordinate is "c"'s position; the first two rows at/after it in [a, b, d, e]
    // are [d, e].
    host.call("c1", &call("public.items.complete", [("id", text("c"))]))
        .expect("complete")
        .commit()
        .expect("commit");
    assert_eq!(
        window(&host, "c1", "w1"),
        ["d", "e"],
        "with the anchor absent, the window starts at the first row at/after c's sort tuple",
    );

    // Insert "bb", which sorts strictly BEFORE the frozen coordinate "c"
    // (a < b < bb < c). The `open` view is now [a, b, bb, d, e]. Per §12.2 the
    // window coordinate is unchanged ("immutable ordered gap"), so the first two
    // rows at/after "c" are still [d, e] — "bb" sorts before the coordinate and
    // cannot be the window's first row.
    host.call("c1", &call("public.items.add", [("id", text("bb"))]))
        .expect("add")
        .commit()
        .expect("commit");

    assert_eq!(
        window(&host, "c1", "w1"),
        ["d", "e"],
        "a row inserted before the frozen sort-tuple coordinate must not enter the anchored gap window",
    );
}
