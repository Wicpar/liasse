#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED (§12.2 bounded-window gap coordinate drops the occurrence-identity tiebreak):
//!
//! SPEC.md §12.2: "If it later leaves the view, the subscription retains its last
//! complete **sort tuple plus occurrence identity** as an immutable ordered gap.
//! That coordinate determines the window until the occurrence reappears or the
//! subscription is reopened."
//!
//! The retained gap is therefore a position in the *total* sort order, and that
//! order appends occurrence identity as its final tiebreaker (SPEC.md §8: "Sort
//! expressions compare lexicographically. Occurrence identity is appended as the
//! final tiebreaker, so repeated occurrences of the same row remain totally
//! ordered."; Annex B.5). `order_rows` in `liasse-runtime` implements exactly
//! this: rows sharing a `$sort` tuple are ordered by `RowId`, which for a
//! top-level keyed row is its canonical key text (Annex D.1/D.2). So while the
//! anchor is absent, the window must begin at the first current row whose
//! `(sort_tuple, occurrence)` pair is **at or after** the frozen anchor's pair —
//! never a row that sorts *before* the anchor's occurrence within an equal-sort
//! group.
//!
//! `crates/liasse-surface/src/window.rs` freezes only the anchor's *sort tuple*
//! (`FrozenGap { coordinate: Vec<Value> }`) and resumes with
//! `partition_point(|row| row.sort_tuple() < self.coordinate)`. It never records
//! the anchor's occurrence identity — even though it already holds it in
//! `Anchor::At(RowId)` and every `ViewRow` exposes `id()`. When several rows
//! share the anchor's sort tuple, *none* of them is strictly `< coordinate`, so
//! `partition_point` returns the **start of the tie group**, placing the window
//! on rows that sort *before* the departed anchor — contradicting §12.2's
//! "at or after" ordered gap.
//!
//! Every item here carries the same `rank`, so the `open` view's sole ordering
//! signal is the B.5 occurrence tiebreak; keyed by text id, that order is fixed
//! and externally deducible: `a < b < c < d < e`.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Precision, RowId, Subscription, SurfaceBinding, SurfaceHost, SurfaceRouter,
    SurfaceRouterBuilder, SurfaceWatch, ViewBinding, ViewRow, VirtualClock, Window,
};
use support::{call, store, text, NOW};

/// Items keyed by a text id, all sharing one `rank`, with an `open` view that
/// drops completed rows and sorts by that (equal) `rank` — so the view order is
/// fixed entirely by the §8/B.5 occurrence tiebreak.
const ITEMS_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.tieitems@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "rank": "int = 5", "done": "bool = false" }
    "index": { "$view": ".items { id, rank, done, $sort: [id] }" }
    "open": { "$view": ".items[:i | !i.done] { id, rank, $sort: [rank] }" }
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
    let engine = Engine::load(store("tieitems"), ITEMS_APP, &mut clock).expect("items app loads");
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
fn absent_anchor_gap_respects_the_occurrence_tiebreak_within_an_equal_sort_group() {
    let mut host = items_host();
    host.connect("c1").unwrap();

    // The `open` view is [a, b, c, d, e] — every row rank 5, ordered solely by
    // the B.5 occurrence (key text) tiebreak. Open a size-2 window anchored on
    // "c": "a concrete anchor normally becomes the first row" -> [c, d].
    let anchor = occurrence(&host, "c");
    let watch =
        SurfaceWatch::new(support::address("public.open"), "w1").with_window(Window::anchored(2, anchor));
    match host.watch("c1", &watch).expect("watch") {
        Subscription::Window(rows) => assert_eq!(ids(&rows), ["c", "d"], "anchor becomes the first row"),
        other => panic!("expected a windowed init, got {other:?}"),
    }

    // Complete "c": the anchor leaves the `open` view, which is now [a, b, d, e]
    // (still every row rank 5). §12.2 freezes the anchor's "sort tuple PLUS
    // occurrence identity" — the pair (rank=5, occurrence=c) — as an immutable
    // ordered gap. The first current rows AT OR AFTER that pair in the total
    // order (§8/B.5: rank then occurrence, a < b < c < d < e) are [d, e]. Rows
    // "a" and "b" sort BEFORE the departed anchor's occurrence and can never be
    // the window's first row.
    host.call("c1", &call("public.items.complete", [("id", text("c"))]))
        .expect("complete")
        .commit()
        .expect("commit");

    assert_eq!(
        window(&host, "c1", "w1"),
        ["d", "e"],
        "an equal-sort-key gap must resume at the first occurrence at/after the departed anchor, not the start of the tie group",
    );
}
