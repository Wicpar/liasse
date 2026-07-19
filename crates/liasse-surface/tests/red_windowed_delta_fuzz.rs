#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 windowed-subscription coherence FUZZER — RED TEAM over commit `ac07db5`.
//!
//! SPEC.md §12.2: "After applying every operation, the client result MUST equal
//! the authorized declared view at the new frontier." For a bounded-window
//! subscription the *client result is its window*, so the ordered patch the runtime
//! emits at each frontier MUST carry the client's prior WINDOW to the new authorized
//! WINDOW — same occurrences, same order, same bounded size, every `$at`/`$to` a
//! position in the current (windowed) result.
//!
//! Commit `ac07db5` made a windowed subscription diff over the WINDOW SLICES
//! (`ViewDelta::between_rows(prior_window, refreshed_window)`) instead of the full
//! view. This fuzzer attacks that fix: it drives a long random stream of real
//! commits (add / move via non-projected sort key / relabel via projected field /
//! relabel+move together / complete / reopen / drop / rekey) through the surface
//! host, holding SEVERAL concurrent bounded windows of different kinds
//! (`first`/`last`/`anchored`/`sliding`, sizes 0..6) over one evolving view, and
//! after every commit asserts, for each window:
//!
//!   apply(prior_window, advance-delta) == INDEPENDENT_ORACLE(authorized full view)
//!
//! The oracle is NOT `Watch::window_rows()` (that would be tautological — the delta
//! is diffed against it). It is a spec-deduced slice of the authorized `open` view
//! the engine recomputes each frontier: `first n` / `last n` / `anchor then n-1` /
//! centered, computed here from the view's own `[rank, id]` order. So a divergence
//! is a genuine §12.2 violation, externally deducible from the view — never the
//! algorithm's own answer.
//!
//! To keep every oracle crisp, the anchored/sliding windows anchor on PROTECTED
//! rows the stream never removes or rekeys (their occurrence is always present, so
//! no ambiguous absent-anchor gap is exercised here — the gap path is pinned by the
//! deterministic `red_windowed_delta_battery` cases and the `red_window_gap_*`
//! suite). A move/complete/drop of a neighbour still slides and evicts on both ends.

mod support;

use std::collections::BTreeMap;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Precision, RowId, SurfaceBinding, SurfaceHost, SurfaceOutcome,
    SurfaceRouter, SurfaceRouterBuilder, Value, ViewBinding, ViewResult, ViewRow,
    VirtualClock, Watch, WatchAuthz, Window,
};
use liasse_value::Integer;
use support::{apply_patch, call, store, text, NOW};

// --- app: `open` = live rows, projected `id`+`label`, sorted `[rank, id]` --------
//
// `rank` is NOT projected, so a rank change is a pure MOVE; `label` IS projected,
// so a label change is a pure in-place UPDATE; both together are update+move on one
// row. `complete`/`reopen` drop/readmit a row from the filtered `open` view
// (membership change without deletion); `drop` deletes; `rekey` changes the key
// (a distinct RowId, and a new `[rank, id]` position).

const APP: &str = r#"{
  "$liasse": 1
  "$app": "example.winfuzz@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "rank": "int = 0", "label": "text = \"x\"", "done": "bool = false" }
    "index": { "$view": ".items { id, rank, label, $sort: [id] }" }
    "open": { "$view": ".items[:i | !i.done] { id, label, $sort: [rank, id] }" }
    "$mut": {
      "add": ".items + { id: @id, rank: @rank, label: @label }"
      "setrank": ".items[@id].rank = @rank"
      "relabel": ".items[@id].label = @label"
      "both": [ ".items[@id].rank = @rank", ".items[@id].label = @label" ]
      "complete": ".items[@id].done = true"
      "reopen": ".items[@id].done = false"
      "drop": ".items - @id"
      "rekey": ".items[@old].id = @new"
    }
    "$public": {
      "items": {
        "$view": ".index"
        "$mut": {
          "add": ".add", "setrank": ".setrank", "relabel": ".relabel", "both": ".both",
          "complete": ".complete", "reopen": ".reopen", "drop": ".drop", "rekey": ".rekey"
        }
      }
      "open": { "$view": ".open" }
    }
  }
}"#;

fn fuzz_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = Engine::load(store("winfuzz"), APP, &mut clock).expect("app loads");
    let router = fuzz_router(engine.model());
    let mut host = SurfaceHost::new(engine, router, clock);
    host.connect("c1").unwrap();
    host
}

fn fuzz_router(model: &liasse_model::Model) -> SurfaceRouter {
    let s = |xs: &[&str]| xs.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    let items = SurfaceBinding::new()
        .with_view(ViewBinding::new("index"))
        .with_call("add", CallBinding::root("add", s(&["id", "rank", "label"])))
        .with_call("setrank", CallBinding::root("setrank", s(&["id", "rank"])))
        .with_call("relabel", CallBinding::root("relabel", s(&["id", "label"])))
        .with_call("both", CallBinding::root("both", s(&["id", "rank", "label"])))
        .with_call("complete", CallBinding::root("complete", s(&["id"])))
        .with_call("reopen", CallBinding::root("reopen", s(&["id"])))
        .with_call("drop", CallBinding::root("drop", s(&["id"])))
        .with_call("rekey", CallBinding::root("rekey", s(&["old", "new"])));
    let open = SurfaceBinding::new().with_view(ViewBinding::new("open"));
    SurfaceRouterBuilder::new()
        .public_surface("items", items)
        .public_surface("open", open)
        .build(model)
        .expect("router validates")
}

fn open_view(host: &SurfaceHost<MemoryStore>) -> ViewResult {
    host.engine().view_at_head("open").expect("view evaluates").expect("open declared")
}

fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}

// The faithful §12.2 windowed client applier is `support::apply_patch` (each
// `$at`/`$to` read in the CURRENT window), shared by every red_* test and backed by
// the one `liasse_wire::apply`.

fn visible(rows: &[ViewRow]) -> Vec<(RowId, BTreeMap<String, Value>)> {
    rows.iter()
        .map(|row| (row.id().clone(), row.fields().map(|(k, v)| (k.clone(), v.clone())).collect()))
        .collect()
}

fn ids(rows: &[ViewRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row.field("id") {
            Some(Value::Text(t)) => t.as_str().to_owned(),
            other => panic!("unexpected id cell {other:?}"),
        })
        .collect()
}

// --- the INDEPENDENT window oracle: a spec-deduced slice of the authorized view ---

#[derive(Clone)]
enum Kind {
    First(usize),
    Last(usize),
    /// anchored on a PROTECTED occurrence (always present): "anchor becomes first".
    Anchored { size: usize, anchor: String },
    /// sliding on a PROTECTED occurrence: "centers it as far as bounds allow".
    Sliding { size: usize, anchor: String },
}

impl Kind {
    fn build(&self, host: &SurfaceHost<MemoryStore>) -> Window {
        match self {
            Self::First(n) => Window::first(*n),
            Self::Last(n) => Window::last(*n),
            Self::Anchored { size, anchor } => Window::anchored(*size, occurrence(host, anchor)),
            Self::Sliding { size, anchor } => Window::anchored(*size, occurrence(host, anchor)).sliding(),
        }
    }

    /// The authorized window as a slice of `full` (the §12.2 authorized declared
    /// view), deduced from the window's own §12.2 selection rule — never from the
    /// runtime's `window_rows()`.
    fn oracle(&self, full: &[ViewRow]) -> Vec<ViewRow> {
        let len = full.len();
        match self {
            Self::First(n) => full[..(*n).min(len)].to_vec(),
            Self::Last(n) => full[len - (*n).min(len)..].to_vec(),
            Self::Anchored { size, anchor } => {
                let pos = pos_of(full, anchor);
                full[pos..(pos + size).min(len)].to_vec()
            }
            Self::Sliding { size, anchor } => {
                let pos = pos_of(full, anchor);
                // §12.2 "centers it as far as the view bounds allow": place the
                // anchor at offset size/2, clamped to [0, len - size].
                let start = pos.saturating_sub(size / 2).min(len.saturating_sub(*size));
                full[start..(start + size).min(len)].to_vec()
            }
        }
    }
}

fn pos_of(full: &[ViewRow], id: &str) -> usize {
    full.iter().position(|row| row.field("id") == Some(&text(id))).unwrap_or_else(|| {
        panic!("protected anchor {id:?} must always be present in the authorized view")
    })
}

/// The stable occurrence identity of the row exposing `id` in `open`.
fn occurrence(host: &SurfaceHost<MemoryStore>, id: &str) -> RowId {
    open_view(host)
        .rows()
        .iter()
        .find(|row| row.field("id") == Some(&text(id)))
        .map(|row| row.id().clone())
        .expect("occurrence present")
}

// --- a tiny deterministic PRNG so a failure reproduces ---------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

// small rank domain -> frequent ties broken by the id/RowId §8/B.5 tiebreak.
const RANKS: [i64; 5] = [0, 1, 2, 3, 4];
const LABELS: [&str; 3] = ["A", "B", "C"];

fn ok(host: &mut SurfaceHost<MemoryStore>, c: &liasse_surface::SurfaceCall) -> bool {
    matches!(host.call("c1", c).expect("call dispatches"), SurfaceOutcome::Committed { .. })
}

#[test]
fn windowed_deltas_stay_coherent_over_many_random_commits() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    let mut host = fuzz_host();

    // Protected anchors: always present, never completed/dropped/rekeyed. Distinct,
    // spread ranks so they interleave with volatile rows across the whole order.
    for (i, a) in ["p0", "p1", "p2", "p3", "p4"].iter().enumerate() {
        ok(
            &mut host,
            &call("public.items.add", [("id", text(a)), ("rank", int(i as i64)), ("label", text("P"))]),
        );
    }
    let protected: Vec<&str> = vec!["p0", "p1", "p2", "p3", "p4"];

    // A battery of concurrent windows of every kind and several sizes.
    let kinds: Vec<Kind> = vec![
        Kind::First(0),
        Kind::First(1),
        Kind::First(2),
        Kind::First(3),
        Kind::First(6),
        Kind::Last(1),
        Kind::Last(2),
        Kind::Last(4),
        Kind::Anchored { size: 1, anchor: "p0".into() },
        Kind::Anchored { size: 2, anchor: "p2".into() },
        Kind::Anchored { size: 3, anchor: "p4".into() },
        Kind::Sliding { size: 3, anchor: "p2".into() },
        Kind::Sliding { size: 5, anchor: "p2".into() },
        Kind::Sliding { size: 4, anchor: "p1".into() },
    ];

    let seq = host.engine().head().unwrap();
    let mut watches: Vec<(Kind, Watch, Vec<ViewRow>)> = kinds
        .into_iter()
        .map(|kind| {
            let mut watch = Watch::windowed("open", WatchAuthz::public(), seq, kind.build(&host));
            watch.init(open_view(&host), seq).expect("window opens");
            let prior = watch.window_rows().expect("windowed rows").to_vec();
            // The init window must itself match the independent oracle.
            let oracle = kind.oracle(open_view(&host).rows());
            assert_eq!(
                visible(&prior),
                visible(&oracle),
                "init window ({:?}) diverged from the authorized slice ({:?})",
                ids(&prior),
                ids(&oracle),
            );
            (kind, watch, prior)
        })
        .collect();

    // Volatile row bookkeeping: which ids are currently live, and which are `done`
    // (present in `items` but filtered out of `open`).
    let mut live: Vec<String> = Vec::new();
    let mut done: Vec<String> = Vec::new();
    let mut counter: u64 = 0;
    let mut checks = 0usize;

    for _round in 0..260 {
        let batch = 1 + rng.below(2);
        for _ in 0..batch {
            mutate(&mut host, &mut rng, &mut live, &mut done, &mut counter, &protected);
        }

        let full = open_view(&host);
        let seq = host.engine().head().unwrap();
        for (kind, watch, prior) in &mut watches {
            let delta = watch.advance(full.clone(), seq);
            let client = apply_patch(prior, &delta);
            let oracle = kind.oracle(full.rows());
            assert_eq!(
                visible(&client),
                visible(&oracle),
                "§12.2 windowed coherence broke: applying the emitted patch to the prior window did \
                 not reproduce the authorized window slice.\n  prior  = {:?}\n  delta  = {delta:?}\n  \
                 client = {:?}\n  oracle = {:?}\n  full   = {:?}",
                ids(prior),
                ids(&client),
                ids(&oracle),
                ids(full.rows()),
            );
            // The runtime's own tracked window must equal the independent oracle too
            // (refresh() must compute the authorized slice, not just a self-consistent one).
            let tracked = watch.window_rows().expect("windowed rows");
            assert_eq!(
                visible(tracked),
                visible(&oracle),
                "the runtime's tracked window {:?} diverged from the authorized slice {:?}",
                ids(tracked),
                ids(&oracle),
            );
            *prior = tracked.to_vec();
            checks += 1;
        }
    }

    assert!(checks > 3000, "the fuzzer must exercise many windowed coherence checks, ran {checks}");
}

fn mutate(
    host: &mut SurfaceHost<MemoryStore>,
    rng: &mut Rng,
    live: &mut Vec<String>,
    done: &mut Vec<String>,
    counter: &mut u64,
    protected: &[&str],
) {
    let want_new = live.is_empty() || rng.below(100) < 30;
    if want_new {
        *counter += 1;
        let id = format!("v{counter}");
        let c = call(
            "public.items.add",
            [("id", text(&id)), ("rank", int(RANKS[rng.below(RANKS.len())])), ("label", text(LABELS[rng.below(LABELS.len())]))],
        );
        if ok(host, &c) {
            live.push(id);
        }
        return;
    }
    let idx = rng.below(live.len());
    let id = live[idx].clone();
    match rng.below(8) {
        0 => {
            let c = call("public.items.setrank", [("id", text(&id)), ("rank", int(RANKS[rng.below(RANKS.len())]))]);
            ok(host, &c);
        }
        1 => {
            let c = call("public.items.relabel", [("id", text(&id)), ("label", text(LABELS[rng.below(LABELS.len())]))]);
            ok(host, &c);
        }
        2 => {
            let c = call(
                "public.items.both",
                [("id", text(&id)), ("rank", int(RANKS[rng.below(RANKS.len())])), ("label", text(LABELS[rng.below(LABELS.len())]))],
            );
            ok(host, &c);
        }
        3 => {
            // complete: leaves the `open` view (still in items).
            let c = call("public.items.complete", [("id", text(&id))]);
            if ok(host, &c) {
                live.remove(idx);
                done.push(id);
            }
        }
        4 => {
            // reopen a completed row, if any.
            if let Some(d) = done.pop() {
                let c = call("public.items.reopen", [("id", text(&d))]);
                if ok(host, &c) {
                    live.push(d);
                }
            }
        }
        5 => {
            let c = call("public.items.drop", [("id", text(&id))]);
            if ok(host, &c) {
                live.remove(idx);
            }
        }
        6 => {
            // rekey a volatile row (never a protected anchor).
            *counter += 1;
            let new = format!("v{counter}");
            let c = call("public.items.rekey", [("old", text(&id)), ("new", text(&new))]);
            if ok(host, &c) {
                live[idx] = new;
            }
        }
        _ => {
            // also occasionally slide/move a PROTECTED anchor (keeps it present).
            let a = protected[rng.below(protected.len())];
            let c = call("public.items.setrank", [("id", text(a)), ("rank", int(RANKS[rng.below(RANKS.len())]))]);
            ok(host, &c);
        }
    }
}
