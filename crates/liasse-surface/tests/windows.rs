#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 bounded windows: `$size`/`$anchor`/`$slide` over a live subscription.
//! The first/last/anchor/slide selections, the non-negative zero-row window, the
//! "exactly one current occurrence at open" requirement, and the immutable
//! ordered gap that tracks an anchor across its disappearance and reappearance —
//! each re-derived from §12.2 text over an items view sorted by a text key.
//!
//! Following an occurrence *across a rekey* (§12.2) is not covered: the engine's
//! [`RowId`] is key-derived, so a rekey changes the tracked identity — that case
//! needs a rekey-stable occurrence identity the runtime does not yet expose.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Precision, RowId, Subscription, SurfaceBinding, SurfaceHost, SurfaceRouter,
    SurfaceRouterBuilder, SurfaceWatch, ViewBinding, ViewRow, VirtualClock, Window,
};
use support::{call, store, text, NOW};

/// Items keyed by a text id, sorted by that id, with a `done` flag and a filtered
/// `open` view — the shapes §12.2's window examples need.
const ITEMS_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.items@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "done": "bool = false" }
    "index": { "$view": ".items { id, done, $sort: [id] }" }
    "open": { "$view": ".items[:i | !i.done] { id, $sort: [id] }" }
    "$mut": {
      "add": ".items + { id: @id }"
      "complete": ".items[@id].done = true"
      "reopen": ".items[@id].done = false"
    }
    "$public": {
      "items": {
        "$view": ".index"
        "$mut": { "add": ".add", "complete": ".complete", "reopen": ".reopen" }
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
    let engine = Engine::load(store("items"), ITEMS_APP, &mut clock).expect("items app loads");
    let router = items_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn items_router(model: &liasse_model::Model) -> SurfaceRouter {
    let items = SurfaceBinding::new()
        .with_view(ViewBinding::new("index"))
        .with_call("add", CallBinding::root("add", ["id".to_owned()]))
        .with_call("complete", CallBinding::root("complete", ["id".to_owned()]))
        .with_call("reopen", CallBinding::root("reopen", ["id".to_owned()]));
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

/// Open a bounded subscription over `target`, expecting it to open, and return its
/// initial windowed ids.
fn open_window(
    host: &mut SurfaceHost<MemoryStore>,
    conn: &str,
    target: &str,
    id: &str,
    win: Window,
) -> Vec<String> {
    let watch = SurfaceWatch::new(support::address(target), id).with_window(win);
    match host.watch(conn, &watch).expect("watch") {
        Subscription::Window(rows) => ids(&rows),
        other => panic!("expected a windowed init, got {other:?}"),
    }
}

/// The stable occurrence identity of the row currently exposing `id` in the
/// `index` view — the anchor an external window request resolves to (§12.2).
fn occurrence(host: &SurfaceHost<MemoryStore>, id: &str) -> RowId {
    let view = host.engine().view_at_head("index").expect("view").expect("declared");
    view.rows()
        .iter()
        .find(|row| row.field("id") == Some(&text(id)))
        .map(|row| row.id().clone())
        .expect("row present")
}

#[test]
fn default_first_and_last_select_the_view_edges() {
    // §12.2: no anchor and `$first` both yield the first `n`; `$last` the last `n`.
    let mut host = items_host();
    host.connect("c1");
    assert_eq!(open_window(&mut host, "c1", "public.items", "w_default", Window::first(2)), ["a", "b"]);
    assert_eq!(open_window(&mut host, "c1", "public.items", "w_last", Window::last(2)), ["d", "e"]);
}

#[test]
fn last_window_tracks_new_tail_rows_and_first_window_does_not() {
    // A commit at the tail moves a `$last` window and leaves a `$first` window's
    // value unchanged (both stay live and advance through the commit).
    let mut host = items_host();
    host.connect("c1");
    open_window(&mut host, "c1", "public.items", "w_first", Window::first(2));
    open_window(&mut host, "c1", "public.items", "w_last", Window::last(2));

    host.call("c1", &call("public.items.add", [("id", text("f"))])).expect("add").commit().expect("commit");

    assert_eq!(window(&host, "c1", "w_first"), ["a", "b"], "the head window is unchanged");
    assert_eq!(window(&host, "c1", "w_last"), ["e", "f"], "the tail window follows the new last rows");
}

#[test]
fn concrete_anchor_becomes_the_first_row() {
    // §12.2: "A concrete anchor normally becomes the first row."
    let mut host = items_host();
    host.connect("c1");
    let anchor = occurrence(&host, "c");
    assert_eq!(open_window(&mut host, "c1", "public.items", "w1", Window::anchored(2, anchor)), ["c", "d"]);
}

#[test]
fn slide_centers_the_anchor_within_the_view_bounds() {
    // §12.2: `$slide: true` centers the anchor as far as the bounds allow. Odd
    // size 3: centered on "c" gives [b, c, d]; on the first row "a" the view start
    // bounds it to [a, b, c].
    let mut host = items_host();
    host.connect("c1");
    let mid = occurrence(&host, "c");
    let start = occurrence(&host, "a");
    assert_eq!(
        open_window(&mut host, "c1", "public.items", "w_mid", Window::anchored(3, mid).sliding()),
        ["b", "c", "d"]
    );
    assert_eq!(
        open_window(&mut host, "c1", "public.items", "w_start", Window::anchored(3, start).sliding()),
        ["a", "b", "c"]
    );
}

#[test]
fn zero_size_window_is_empty_and_stays_live() {
    // §12.2: `$size` is a non-negative count, so 0 is a valid, permanently empty,
    // still-live window whose frontier still advances through same-connection
    // commits.
    let mut host = items_host();
    host.connect("c1");
    assert!(open_window(&mut host, "c1", "public.items", "w0", Window::first(0)).is_empty());

    host.call("c1", &call("public.items.add", [("id", text("f"))])).expect("add").commit().expect("commit");
    assert!(window(&host, "c1", "w0").is_empty(), "the zero-row window is still empty at the advanced frontier");
}

#[test]
fn anchor_with_no_current_occurrence_fails_to_open() {
    // §12.2: "The anchor MUST identify exactly one current occurrence when the
    // window opens." A key never inserted identifies zero occurrences, so opening
    // fails — not an authorization refusal.
    let mut host = items_host();
    host.connect("c1");
    let watch = SurfaceWatch::new(support::address("public.items"), "w1")
        .with_window(Window::anchored(2, RowId::keyed("zz")));
    match host.watch("c1", &watch).expect("watch") {
        Subscription::Failed(_) => {}
        other => panic!("expected a window open failure, got {other:?}"),
    }
    assert!(host.read_window("c1", "w1").is_none(), "a window that failed to open installs no subscription");
}

#[test]
fn anchor_gap_persists_then_reanchors_on_reappearance() {
    // §12.2: when the anchored occurrence leaves the view the window is determined
    // by the immutable gap coordinate (first rows at or after it); when the same
    // occurrence reappears the window anchors on it again. The `open` view drops a
    // completed row and readmits a reopened one.
    let mut host = items_host();
    host.connect("c1");
    let anchor = occurrence(&host, "b");
    assert_eq!(open_window(&mut host, "c1", "public.open", "w1", Window::anchored(2, anchor)), ["b", "c"]);

    // Complete "b": it leaves the open view. The gap coordinate (just after "a")
    // determines the window: the first two rows at or after it are "c" and "d".
    host.call("c1", &call("public.items.complete", [("id", text("b"))])).expect("complete").commit().unwrap();
    assert_eq!(window(&host, "c1", "w1"), ["c", "d"], "the gap coordinate holds the window while the anchor is absent");

    // Reopen "b": the same occurrence re-enters and the window anchors on it again.
    host.call("c1", &call("public.items.reopen", [("id", text("b"))])).expect("reopen").commit().unwrap();
    assert_eq!(window(&host, "c1", "w1"), ["b", "c"], "the reappeared occurrence re-anchors the window");
}
