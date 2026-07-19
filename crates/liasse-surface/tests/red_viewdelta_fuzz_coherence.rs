#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 ordered-patch coherence FUZZER over a sorted, projected view.
//!
//! SPEC.md §12.2: "$at and $to are zero-based positions in the current result.
//! update replaces the occurrence value while preserving identity. ... After
//! applying every operation, the client result MUST equal the authorized declared
//! view at the new frontier."
//!
//! This drives a long random stream of real commits (add / relabel /
//! reprioritize / relabel+reprioritize together / remove / rekey) through the
//! surface host, snapshots the authorized `listing` view after each batch, and —
//! for every ordered pair of snapshots (prev earlier than next) it checks —
//! asserts that a FAITHFUL §12.2 client that applies `ViewDelta::between(prev,
//! next)` op-by-op (each position read in the current mid-application result)
//! reproduces `next` EXACTLY: same occurrences, same exposed values, same order.
//!
//! The expected result is externally deducible: it is `next` itself (the
//! recomputed authorized view), never the algorithm's own output. The sort is
//! `[prio, name]` with `prio` NOT projected, so a `prio` change is a pure move, a
//! `label` change is a pure in-place update, a `label`+`prio` change is an
//! update+move on ONE row (does the changed value land at the moved position?),
//! and a `name` change is a rekey rendered as remove+insert. A small `prio`
//! domain forces frequent ties broken by `name`, exercising adjacent-tie reorder.

mod support;

use std::collections::BTreeMap;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Precision, RowId, SurfaceBinding, SurfaceHost, SurfaceOutcome,
    SurfaceRouter, SurfaceRouterBuilder, Value, ViewBinding, ViewDelta, ViewResult, ViewRow,
    VirtualClock,
};
use liasse_value::Integer;
use support::{apply_patch, call, store, text, NOW};

// The faithful §12.2 client applier is `support::apply_patch`, shared by every
// red_* test and backed by the one `liasse_wire::apply`.

/// The client-visible content: each occurrence identity + exposed fields, in
/// order. This is exactly what §12.2 requires the client result to equal.
fn visible(rows: &[ViewRow]) -> Vec<(RowId, BTreeMap<String, Value>)> {
    rows.iter()
        .map(|row| (row.id().clone(), row.fields().map(|(k, v)| (k.clone(), v.clone())).collect()))
        .collect()
}

// --- app: `.items { name, label, $sort: [prio, name] }` (prio NOT projected) ---

const FUZZ_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.fuzzcases@1.0.0"
  "$model": {
    "items": { "$key": "name", "name": "text", "label": "text", "prio": "int = 0" }
    "listing": { "$view": ".items { name, label, $sort: [prio, name] }" }
    "$mut": {
      "add": ".items + { name: @name, label: @label, prio: @prio }"
      "relabel": ".items[@name].label = @label"
      "reprio": ".items[@name].prio = @prio"
      "both": [ ".items[@name].label = @label", ".items[@name].prio = @prio" ]
      "drop": ".items - @name"
      "rekey": ".items[@old].name = @new"
    }
    "$public": {
      "items": {
        "$view": ".listing"
        "$mut": {
          "add": ".add", "relabel": ".relabel", "reprio": ".reprio",
          "both": ".both", "drop": ".drop", "rekey": ".rekey"
        }
      }
    }
  }
}"#;

fn fuzz_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = Engine::load(store("fuzzcases"), FUZZ_APP, &mut clock).expect("fuzz app loads");
    let router = fuzz_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn fuzz_router(model: &liasse_model::Model) -> SurfaceRouter {
    let strs = |names: &[&str]| names.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let items = SurfaceBinding::new()
        .with_view(ViewBinding::new("listing"))
        .with_call("add", CallBinding::root("add", strs(&["name", "label", "prio"])))
        .with_call("relabel", CallBinding::root("relabel", strs(&["name", "label"])))
        .with_call("reprio", CallBinding::root("reprio", strs(&["name", "prio"])))
        .with_call("both", CallBinding::root("both", strs(&["name", "label", "prio"])))
        .with_call("drop", CallBinding::root("drop", strs(&["name"])))
        .with_call("rekey", CallBinding::root("rekey", strs(&["old", "new"])));
    SurfaceRouterBuilder::new()
        .public_surface("items", items)
        .build(model)
        .expect("router validates against the fuzz model")
}

fn listing(host: &SurfaceHost<MemoryStore>) -> ViewResult {
    host.engine().view_at_head("listing").expect("view evaluates").expect("listing declared")
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

// --- a tiny deterministic PRNG so a failure is reproducible --------------------

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
    fn label(&mut self) -> &'static str {
        LABELS[self.below(LABELS.len())]
    }
    fn prio(&mut self) -> i64 {
        PRIOS[self.below(PRIOS.len())]
    }
}

const LABELS: [&str; 3] = ["A", "B", "C"];
const PRIOS: [i64; 4] = [0, 1, 2, 3];

fn ok(host: &mut SurfaceHost<MemoryStore>, c: &liasse_surface::SurfaceCall) -> bool {
    matches!(host.call("c1", c).expect("call dispatches"), SurfaceOutcome::Committed { .. })
}

#[test]
fn fuzz_ordered_patch_stays_coherent_over_many_random_commits() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut host = fuzz_host();
    host.connect("c1").unwrap();

    let mut live: Vec<String> = Vec::new();
    let mut counter: u64 = 0;
    // A rolling history of (prev-result) snapshots to diff future states against.
    let mut snapshots: Vec<ViewResult> = vec![listing(&host)];

    let mut checks = 0usize;
    // A few hundred rounds keep this well under a few seconds while still
    // exercising inserts, removes, in-place updates, moves, update+move, rekeys,
    // sort ties, and non-adjacent-frontier patches many hundreds of times.
    for _round in 0..300 {
        // One or two mutations per round, then snapshot + coherence-check.
        let batch = 1 + rng.below(2);
        for _ in 0..batch {
            let choose_new = live.is_empty() || rng.below(100) < 35;
            if choose_new {
                counter += 1;
                let name = format!("n{counter}");
                let c = call(
                    "public.items.add",
                    [("name", text(&name)), ("label", text(rng.label())), ("prio", int(rng.prio()))],
                );
                if ok(&mut host, &c) {
                    live.push(name);
                }
                continue;
            }
            let idx = rng.below(live.len());
            let name = live[idx].clone();
            match rng.below(5) {
                0 => {
                    let c = call("public.items.relabel", [("name", text(&name)), ("label", text(rng.label()))]);
                    ok(&mut host, &c);
                }
                1 => {
                    let c = call("public.items.reprio", [("name", text(&name)), ("prio", int(rng.prio()))]);
                    ok(&mut host, &c);
                }
                2 => {
                    let c = call(
                        "public.items.both",
                        [("name", text(&name)), ("label", text(rng.label())), ("prio", int(rng.prio()))],
                    );
                    ok(&mut host, &c);
                }
                3 => {
                    let c = call("public.items.drop", [("name", text(&name))]);
                    if ok(&mut host, &c) {
                        live.remove(idx);
                    }
                }
                _ => {
                    counter += 1;
                    let new = format!("n{counter}");
                    let c = call("public.items.rekey", [("old", text(&name)), ("new", text(&new))]);
                    if ok(&mut host, &c) {
                        live[idx] = new;
                    }
                }
            }
        }

        let next = listing(&host);

        // Check coherence against the immediately-previous snapshot AND a random
        // earlier one (a multi-commit patch between non-adjacent frontiers).
        let prev = snapshots.last().expect("at least one prior snapshot");
        assert_coherent(prev, &next, checks);
        checks += 1;
        if snapshots.len() > 1 {
            let older = &snapshots[rng.below(snapshots.len())];
            assert_coherent(older, &next, checks);
            checks += 1;
        }

        snapshots.push(next);
        if snapshots.len() > 24 {
            snapshots.remove(0);
        }
    }

    assert!(checks > 400, "the fuzzer must exercise many coherence checks, ran {checks}");
}

fn assert_coherent(prev: &ViewResult, next: &ViewResult, check: usize) {
    let delta = ViewDelta::between(Some(prev), next);
    let client = apply_patch(prev.rows(), &delta);
    assert_eq!(
        visible(&client),
        visible(next.rows()),
        "check #{check}: §12.2 — after applying every op the client result MUST equal the \
         authorized declared view at the new frontier (order included).\n  prev = {:?}\n  next = {:?}\n  delta = {:?}",
        visible(prev.rows()),
        visible(next.rows()),
        delta,
    );
}
