#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 windowed-subscription coherence — TARGETED RED-TEAM battery over commit
//! `ac07db5` (windowed subscriptions emit a window-relative `ViewDelta`).
//!
//! SPEC.md §12.2: "After applying every operation, the client result MUST equal the
//! authorized declared view at the new frontier." For a bounded window the client
//! result is its WINDOW, so after applying `advance`'s ordered patch to the prior
//! window the client MUST hold the new authorized window — same occurrences, same
//! order, same bounded size, every `$at`/`$to` a position in the current windowed
//! result.
//!
//! Each probe here drives `Watch::windowed` → `init` → `advance`, applies the
//! emitted delta to the PRIOR window with a faithful §12.2 client, and asserts the
//! result equals an EXTERNALLY DEDUCIBLE window — fixed by the view's `[rank, id]`
//! order and the window's `$size`/`$anchor`/`$slide` rule, never by the runtime's
//! own `window_rows()`. It also asserts the runtime's tracked window equals that
//! same independent expectation, so a `refresh()` that computed a merely
//! self-consistent (but non-authorized) window would be caught.
//!
//! Attack angles: move inside↔outside a window (eviction / re-entry at the right
//! position); update of the last window row; a tie-broken boundary insertion
//! (§8/B.5 RowId tiebreak decides membership); a both-ends eviction in one commit;
//! a pure move-within reordering two in-window rows; size-1, size>result, and
//! empty↔nonempty transitions; §12.3 duplicate-advance idempotency; a non-sliding
//! absent-anchor gap resume; sliding re-centering. The final probe PINS the current
//! sliding + absent-anchor behavior and flags it as the one under-specified seam.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, PatchOp, Precision, RowId, SurfaceBinding, SurfaceHost, SurfaceRouter,
    SurfaceRouterBuilder, Value, ViewBinding, ViewDelta, ViewResult, ViewRow, VirtualClock, Watch,
    WatchAuthz, Window,
};
use liasse_value::Integer;
use support::{apply_patch, call, store, text, NOW};

const APP: &str = r#"{
  "$liasse": 1
  "$app": "example.winbattery@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "rank": "int = 0", "label": "text = \"x\"", "done": "bool = false" }
    "index": { "$view": ".items { id, rank, label, $sort: [id] }" }
    "open": { "$view": ".items[:i | !i.done] { id, label, $sort: [rank, id] }" }
    "$mut": {
      "add": ".items + { id: @id, rank: @rank, label: @label }"
      "setrank": ".items[@id].rank = @rank"
      "relabel": ".items[@id].label = @label"
      "complete": ".items[@id].done = true"
      "drop": ".items - @id"
    }
    "$public": {
      "items": {
        "$view": ".index"
        "$mut": {
          "add": ".add", "setrank": ".setrank", "relabel": ".relabel",
          "complete": ".complete", "drop": ".drop"
        }
      }
      "open": { "$view": ".open" }
    }
  }
}"#;

fn open_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = Engine::load(store("winbattery"), APP, &mut clock).expect("app loads");
    let router = router(engine.model());
    let mut host = SurfaceHost::new(engine, router, clock);
    host.connect("c1");
    host
}

fn router(model: &liasse_model::Model) -> SurfaceRouter {
    let s = |xs: &[&str]| xs.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    let items = SurfaceBinding::new()
        .with_view(ViewBinding::new("index"))
        .with_call("add", CallBinding::root("add", s(&["id", "rank", "label"])))
        .with_call("setrank", CallBinding::root("setrank", s(&["id", "rank"])))
        .with_call("relabel", CallBinding::root("relabel", s(&["id", "label"])))
        .with_call("complete", CallBinding::root("complete", s(&["id"])))
        .with_call("drop", CallBinding::root("drop", s(&["id"])));
    let open = SurfaceBinding::new().with_view(ViewBinding::new("open"));
    SurfaceRouterBuilder::new()
        .public_surface("items", items)
        .public_surface("open", open)
        .build(model)
        .expect("router validates")
}

fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}

fn open_view(host: &SurfaceHost<MemoryStore>) -> ViewResult {
    host.engine().view_at_head("open").expect("view evaluates").expect("open declared")
}

fn add(host: &mut SurfaceHost<MemoryStore>, id: &str, rank: i64) {
    let c = call("public.items.add", [("id", text(id)), ("rank", int(rank)), ("label", text("x"))]);
    host.call("c1", &c).expect("dispatch").commit().expect("commit");
}
fn setrank(host: &mut SurfaceHost<MemoryStore>, id: &str, rank: i64) {
    let c = call("public.items.setrank", [("id", text(id)), ("rank", int(rank))]);
    host.call("c1", &c).expect("dispatch").commit().expect("commit");
}
fn relabel(host: &mut SurfaceHost<MemoryStore>, id: &str, label: &str) {
    let c = call("public.items.relabel", [("id", text(id)), ("label", text(label))]);
    host.call("c1", &c).expect("dispatch").commit().expect("commit");
}
fn complete(host: &mut SurfaceHost<MemoryStore>, id: &str) {
    let c = call("public.items.complete", [("id", text(id))]);
    host.call("c1", &c).expect("dispatch").commit().expect("commit");
}
fn drop_row(host: &mut SurfaceHost<MemoryStore>, id: &str) {
    let c = call("public.items.drop", [("id", text(id))]);
    host.call("c1", &c).expect("dispatch").commit().expect("commit");
}

fn occurrence(host: &SurfaceHost<MemoryStore>, id: &str) -> RowId {
    open_view(host)
        .rows()
        .iter()
        .find(|row| row.field("id") == Some(&text(id)))
        .map(|row| row.id().clone())
        .expect("occurrence present")
}

fn ids(rows: &[ViewRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row.field("id") {
            Some(Value::Text(t)) => t.as_str().to_owned(),
            other => panic!("unexpected id cell {other:?}"),
        })
        .collect()
}

fn labels(rows: &[ViewRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row.field("label") {
            Some(Value::Text(t)) => t.as_str().to_owned(),
            other => panic!("unexpected label cell {other:?}"),
        })
        .collect()
}

// A faithful §12.2 windowed client — each `$at`/`$to` read in the CURRENT windowed
// result; a position outside it is a malformed windowed patch — is
// `support::apply_patch`, shared by every red_* test and backed by the one
// `liasse_wire::apply`.

/// Advance `watch` to the current authorized view, apply the emitted delta to
/// `prior` with a faithful client, and assert BOTH the client result AND the
/// runtime's tracked window equal `expected` (an externally deducible id list).
/// Returns the new prior window.
fn step(
    host: &SurfaceHost<MemoryStore>,
    watch: &mut Watch,
    prior: &[ViewRow],
    expected: &[&str],
) -> Vec<ViewRow> {
    let seq = host.engine().head();
    let delta = watch.advance(open_view(host), seq);
    let client = apply_patch(prior, &delta);
    let tracked = watch.window_rows().expect("windowed rows").to_vec();
    assert_eq!(
        ids(&tracked),
        expected,
        "refresh() must compute the authorized window slice; got {:?}, expected {expected:?}",
        ids(&tracked),
    );
    assert_eq!(
        ids(&client),
        expected,
        "§12.2: after applying the emitted patch the client WINDOW must equal the authorized window \
         {expected:?}; got {:?} (delta {delta:?})",
        ids(&client),
    );
    tracked
}

fn open_first(host: &SurfaceHost<MemoryStore>, size: usize) -> (Watch, Vec<ViewRow>) {
    open_with(host, Window::first(size))
}

fn open_with(host: &SurfaceHost<MemoryStore>, window: Window) -> (Watch, Vec<ViewRow>) {
    let seq = host.engine().head();
    let mut watch = Watch::windowed("open", WatchAuthz::public(), seq, window);
    watch.init(open_view(host), seq).expect("window opens");
    let prior = watch.window_rows().expect("windowed rows").to_vec();
    (watch, prior)
}

// --- probes ----------------------------------------------------------------------

#[test]
fn move_from_inside_to_outside_evicts_and_pulls_the_next_row_in() {
    // open = [a, b, c, d, e]; first(3) window = [a, b, c].
    let mut host = open_host();
    for (i, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
        add(&mut host, id, i as i64);
    }
    let (mut watch, prior) = open_first(&host, 3);
    assert_eq!(ids(&prior), ["a", "b", "c"]);

    // Move "c" past "e" (rank 9). Full view [a, b, d, e, c]; first(3) = [a, b, d].
    setrank(&mut host, "c", 9);
    step(&host, &mut watch, &prior, &["a", "b", "d"]);
}

#[test]
fn move_from_outside_to_inside_inserts_at_the_right_window_position() {
    // open = [a, b, c, d, e]; first(3) = [a, b, c].
    let mut host = open_host();
    for (i, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
        add(&mut host, id, (i as i64) * 2);
    }
    let (mut watch, prior) = open_first(&host, 3);
    assert_eq!(ids(&prior), ["a", "b", "c"]);

    // Move "e" to rank 1 (between a=0 and b=2). Full [a, e, b, c, d]; first(3) = [a, e, b].
    setrank(&mut host, "e", 1);
    step(&host, &mut watch, &prior, &["a", "e", "b"]);
}

#[test]
fn update_of_the_last_window_row_is_an_in_place_update() {
    // first(3) = [a, b, c]; relabel c -> a projected value change at the last slot.
    let mut host = open_host();
    for (i, id) in ["a", "b", "c", "d"].iter().enumerate() {
        add(&mut host, id, i as i64);
    }
    let (mut watch, prior) = open_first(&host, 3);
    assert_eq!(labels(&prior), ["x", "x", "x"]);

    relabel(&mut host, "c", "Z");
    let seq = host.engine().head();
    let delta = watch.advance(open_view(&host), seq);
    assert!(
        matches!(&delta, ViewDelta::Patch(ops) if ops.len() == 1 && matches!(ops[0], PatchOp::Update { .. })),
        "a value-only change at an in-window row is a single position-preserving `update`, got {delta:?}",
    );
    let client = apply_patch(&prior, &delta);
    assert_eq!(ids(&client), ["a", "b", "c"], "identity and order preserved");
    assert_eq!(labels(&client), ["x", "x", "Z"], "the last window row carries its new value");
    assert_eq!(labels(watch.window_rows().unwrap()), ["x", "x", "Z"]);
}

#[test]
fn boundary_tie_insertion_membership_is_decided_by_the_rowid_tiebreak() {
    // Ranks tie at 1; the §8/B.5 RowId (key text) tiebreak orders equal ranks by id.
    // open = [a(0), m(1), q(1)]; first(2) = [a, m]  (m before q by id).
    let mut host = open_host();
    add(&mut host, "a", 0);
    add(&mut host, "m", 1);
    add(&mut host, "q", 1);
    let (mut watch, prior) = open_first(&host, 2);
    assert_eq!(ids(&prior), ["a", "m"]);

    // Insert "b" at rank 1: among rank-1 rows the id order is b < m < q, so the total
    // order is [a, b, m, q] and "b" ENTERS the first(2) window, evicting "m".
    add(&mut host, "b", 1);
    let prior = step(&host, &mut watch, &prior, &["a", "b"]);

    // Insert "z" at rank 1: id order b < m < q < z, so "z" sorts AFTER the boundary
    // and does NOT enter — the window is unchanged, a frontier-only (empty) patch.
    add(&mut host, "z", 1);
    let seq = host.engine().head();
    let delta = watch.advance(open_view(&host), seq);
    assert_eq!(delta, ViewDelta::Patch(vec![]), "a tie sorting after the boundary is frontier-only, got {delta:?}");
    let client = apply_patch(&prior, &delta);
    assert_eq!(ids(&client), ["a", "b"], "the window is unchanged");
}

#[test]
fn both_ends_evict_in_a_single_commit() {
    // first(3) = [a, b, c]; move the head "a" past the tail so the window's front row
    // leaves AND a new row enters the back in ONE commit.
    let mut host = open_host();
    for (i, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
        add(&mut host, id, i as i64);
    }
    let (mut watch, prior) = open_first(&host, 3);
    assert_eq!(ids(&prior), ["a", "b", "c"]);

    // Move "a" to rank 9: full view [b, c, d, e, a]; first(3) = [b, c, d].
    setrank(&mut host, "a", 9);
    step(&host, &mut watch, &prior, &["b", "c", "d"]);
}

#[test]
fn move_within_the_window_reorders_two_in_window_rows() {
    // first(4) = [a, b, c, d]; move "c" ahead of "b" so two in-window rows swap
    // WITHOUT any eviction — a pure `move` (rank is not projected).
    let mut host = open_host();
    for (i, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
        add(&mut host, id, (i as i64) * 2 + 2); // a=2,b=4,c=6,d=8,e=10
    }
    let (mut watch, prior) = open_first(&host, 4);
    assert_eq!(ids(&prior), ["a", "b", "c", "d"]);

    // Move "c" to rank 3 (between a=2 and b=4). Full [a, c, b, d, e]; first(4) = [a, c, b, d].
    setrank(&mut host, "c", 3);
    let seq = host.engine().head();
    let delta = watch.advance(open_view(&host), seq);
    assert!(
        matches!(&delta, ViewDelta::Patch(ops) if ops.iter().all(|o| matches!(o, PatchOp::Move { .. }))),
        "a reorder of two in-window rows is move-only (no eviction), got {delta:?}",
    );
    let client = apply_patch(&prior, &delta);
    assert_eq!(ids(&client), ["a", "c", "b", "d"], "the two rows swapped inside the window");
    assert_eq!(ids(watch.window_rows().unwrap()), ["a", "c", "b", "d"]);
}

#[test]
fn size_one_window_tracks_the_head() {
    let mut host = open_host();
    add(&mut host, "a", 0);
    add(&mut host, "b", 1);
    let (mut watch, prior) = open_first(&host, 1);
    assert_eq!(ids(&prior), ["a"]);

    // Move "a" to the back: the size-1 head window becomes [b].
    setrank(&mut host, "a", 9);
    step(&host, &mut watch, &prior, &["b"]);
}

#[test]
fn window_larger_than_the_result_never_evicts() {
    let mut host = open_host();
    add(&mut host, "a", 0);
    add(&mut host, "b", 1);
    let (mut watch, prior) = open_first(&host, 5);
    assert_eq!(ids(&prior), ["a", "b"]);

    add(&mut host, "c", 2);
    step(&host, &mut watch, &prior, &["a", "b", "c"]);
}

#[test]
fn empty_to_nonempty_and_back_to_empty() {
    let mut host = open_host();
    let (mut watch, prior) = open_first(&host, 2);
    assert!(prior.is_empty(), "the window opens empty over an empty view");

    add(&mut host, "a", 0);
    add(&mut host, "b", 1);
    let prior = step(&host, &mut watch, &prior, &["a", "b"]);

    drop_row(&mut host, "a");
    drop_row(&mut host, "b");
    step(&host, &mut watch, &prior, &[]);
}

#[test]
fn duplicate_advance_does_not_double_apply() {
    // §12.3: a retried advance at the same frontier must be a no-op, not a re-apply.
    let mut host = open_host();
    add(&mut host, "a", 0);
    add(&mut host, "b", 1);
    add(&mut host, "c", 2);
    let (mut watch, prior) = open_first(&host, 2);
    assert_eq!(ids(&prior), ["a", "b"]);

    // A front insert evicts "b": window -> [a0, ...]. Add "aa" at rank 0 (id a < aa).
    add(&mut host, "aa", 0);
    let seq = host.engine().head();
    let first = watch.advance(open_view(&host), seq);
    let after_first = apply_patch(&prior, &first);
    assert_eq!(ids(&after_first), ["a", "aa"]);

    // Retry the SAME advance (same result, same frontier): must be frontier-only.
    let retry = watch.advance(open_view(&host), seq);
    assert_eq!(retry, ViewDelta::Patch(vec![]), "a duplicate advance must be an empty patch, got {retry:?}");
    let after_retry = apply_patch(&after_first, &retry);
    assert_eq!(ids(&after_retry), ["a", "aa"], "the retry must not double-apply the eviction/insert");
}

#[test]
fn non_sliding_absent_anchor_gap_resumes_at_first_rows_at_or_after() {
    // Anchored (non-slide) size 2 on "c" over [a, b, c, d, e] -> [c, d]. When "c"
    // leaves, §12.2 freezes (sort tuple, occurrence) = (rank=2, c) and the window is
    // "the first rows at or after it": in [a, b, d, e] that is [d, e]. Externally
    // deducible from the [rank, id] order; the emitted delta must reproduce it.
    let mut host = open_host();
    for (i, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
        add(&mut host, id, i as i64);
    }
    let anchor = occurrence(&host, "c");
    let (mut watch, prior) = open_with(&host, Window::anchored(2, anchor));
    assert_eq!(ids(&prior), ["c", "d"], "a concrete anchor becomes the first row");

    complete(&mut host, "c");
    step(&host, &mut watch, &prior, &["d", "e"]);
}

#[test]
fn sliding_present_recenters_when_the_anchor_moves() {
    // Sliding size 3 on "c" over [a, b, c, d, e]: centered -> [b, c, d]. Move "c" to
    // the tail: full [a, b, d, e, c], "c" at index 4; "centered as far as bounds
    // allow" clamps the size-3 frame to the last three rows -> [e, c] tail... i.e.
    // start = clamp(4 - 1, 0, 5 - 3) = 2 -> [d, e, c].
    let mut host = open_host();
    for (i, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
        add(&mut host, id, i as i64);
    }
    let anchor = occurrence(&host, "c");
    let (mut watch, prior) = open_with(&host, Window::anchored(3, anchor).sliding());
    assert_eq!(ids(&prior), ["b", "c", "d"], "the anchor is centered");

    setrank(&mut host, "c", 9);
    step(&host, &mut watch, &prior, &["d", "e", "c"]);
}

#[test]
fn sliding_absent_anchor_gap_is_first_rows_at_or_after_documented_seam() {
    // SEAM (under-specified in §12.2): a SLIDING window centers a PRESENT anchor,
    // but once the anchor leaves, `Window::select` takes the absent-anchor branch
    // and resumes at "first rows at/after the frozen gap" WITHOUT re-centering — the
    // `$slide` flag is ignored while the anchor is gone (window.rs: `center()` is
    // applied only in the present branch). §12.2 says only that the frozen coordinate
    // "determines the window", not whether a sliding window re-centers on the gap, so
    // this is a spec ambiguity, not a clear violation. This probe PINS the current
    // behavior so a future change is a conscious decision, and the delta stays
    // coherent regardless.
    let mut host = open_host();
    for (i, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
        add(&mut host, id, i as i64);
    }
    let anchor = occurrence(&host, "c");
    let (mut watch, prior) = open_with(&host, Window::anchored(3, anchor).sliding());
    assert_eq!(ids(&prior), ["b", "c", "d"], "present sliding window is centered on the anchor");

    // Complete "c": the anchor leaves. Full view [a, b, d, e]; the frozen gap is
    // (rank=2, c). "First rows at/after" -> [d, e]. A hypothetical re-centered
    // reading would instead retain a row before the gap (e.g. [b, d, e]); the runtime
    // does NOT do that. Delta coherence (apply == tracked) holds either way.
    complete(&mut host, "c");
    let seq = host.engine().head();
    let delta = watch.advance(open_view(&host), seq);
    let client = apply_patch(&prior, &delta);
    let tracked = watch.window_rows().expect("windowed rows").to_vec();
    assert_eq!(ids(&tracked), ["d", "e"], "current sliding-gap behavior: first rows at/after, not re-centered");
    assert_eq!(ids(&client), ids(&tracked), "§12.2: the emitted delta reproduces the runtime window exactly");
}
