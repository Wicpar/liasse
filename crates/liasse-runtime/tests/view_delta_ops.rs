#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Direct op-sequence tests for `ViewDelta::between` (SPEC.md §12.2).
//!
//! §12.2 fixes the live-view patch as an ordered sequence of `insert { $at }`,
//! `remove`, `move { $to }`, `update`, `rekey` operations, with positions read in
//! the current result. These tests pin the exact op sequence `between` computes
//! for each kind of change over the sorted `listing` view — each expectation
//! deducible from the view's `$sort: [prio, name]` and §12.2 alone.

mod support;

use liasse_runtime::{
    CallOutcome, CallRequest, Engine, PatchOp, Value, ViewDelta, ViewResult,
};
use liasse_store::MemoryStore;
use liasse_value::{Integer, Text};
use support::{generator, load};

/// Items keyed by `name`, exposing `{ name, label }`, ordered by `[prio, name]` —
/// `prio` is NOT exposed, so a `prio` change reorders without changing the
/// exposed value.
const APP: &str = r#"{
  "$liasse": 1
  "$app": "t.viewdeltaops@1.0.0"
  "$model": {
    "items": { "$key": "name", "name": "text", "label": "text = 'base'", "prio": "int = 0" }
    "listing": { "$view": ".items { name, label, $sort: [prio, name] }" }
    "$mut": {
      "add": ".items + { name: @name, prio: @prio }"
      "setprio": ".items[@name].prio = @prio"
      "setlabel": ".items[@name].label = @label"
      "rekey": ".items[@old].name = @new"
      "remove": ".items - @name"
    }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

fn commit(engine: &mut Engine<MemoryStore>, request: CallRequest) {
    let mut g = generator();
    match engine.call(&request, &mut g).expect("engine is fault-free") {
        CallOutcome::Committed { .. } => {}
        other => panic!("expected a committed mutation, got {other:?}"),
    }
}

fn listing(engine: &Engine<MemoryStore>) -> ViewResult {
    engine.view_at_head("listing").expect("view evaluates").expect("listing declared")
}

fn add(engine: &mut Engine<MemoryStore>, name: &str, prio: i64) {
    commit(engine, CallRequest::new("add").arg("name", text(name)).arg("prio", int(prio)));
}

fn ops(prev: &ViewResult, next: &ViewResult) -> Vec<PatchOp> {
    match ViewDelta::between(Some(prev), next) {
        ViewDelta::Patch(ops) => ops,
        ViewDelta::Init(_) => panic!("between(Some, _) is a patch, not an init"),
    }
}

#[test]
fn first_observation_is_an_init() {
    let e = load("vdo_init", APP);
    let result = listing(&e);
    assert!(matches!(ViewDelta::between(None, &result), ViewDelta::Init(_)));
}

#[test]
fn an_unchanged_view_is_a_frontier_only_empty_patch() {
    // §12.2: "A frontier-only patch has an empty operation sequence."
    let mut e = load("vdo_frontier", APP);
    add(&mut e, "a", 0);
    let prev = listing(&e);
    let next = listing(&e);
    assert!(ops(&prev, &next).is_empty(), "an unchanged view produces no operations");
}

#[test]
fn front_insert_is_insert_at_zero() {
    let mut e = load("vdo_front", APP);
    add(&mut e, "m", 1);
    let prev = listing(&e);
    add(&mut e, "a", 0); // a sorts before m
    let next = listing(&e);
    let ops = ops(&prev, &next);
    assert!(matches!(ops.as_slice(), [PatchOp::Insert { at: 0, .. }]), "{ops:?}");
}

#[test]
fn middle_insert_is_insert_at_the_sorted_index() {
    let mut e = load("vdo_mid", APP);
    add(&mut e, "a", 0);
    add(&mut e, "c", 2);
    let prev = listing(&e); // [a, c]
    add(&mut e, "b", 1); // between a and c
    let next = listing(&e); // [a, b, c]
    let ops = ops(&prev, &next);
    assert!(matches!(ops.as_slice(), [PatchOp::Insert { at: 1, .. }]), "{ops:?}");
}

#[test]
fn pure_reorder_is_a_single_move_and_no_update() {
    // `prio` is not exposed, so bumping it reorders without changing `{name,label}`.
    let mut e = load("vdo_reorder", APP);
    add(&mut e, "a", 0);
    add(&mut e, "b", 1);
    let prev = listing(&e); // [a, b]
    commit(&mut e, CallRequest::new("setprio").arg("name", text("a")).arg("prio", int(5)));
    let next = listing(&e); // [b, a]
    let ops = ops(&prev, &next);
    assert!(matches!(ops.as_slice(), [PatchOp::Move { to: 0, .. }]), "a lone move to the front, {ops:?}");
}

#[test]
fn value_only_change_is_a_single_in_place_update() {
    let mut e = load("vdo_update", APP);
    add(&mut e, "a", 0);
    add(&mut e, "b", 1);
    let prev = listing(&e); // [a, b], labels "base"
    commit(&mut e, CallRequest::new("setlabel").arg("name", text("a")).arg("label", text("X")));
    let next = listing(&e); // [a("X"), b] — order unchanged
    let ops = ops(&prev, &next);
    assert!(matches!(ops.as_slice(), [PatchOp::Update { .. }]), "a lone in-place update, {ops:?}");
}

#[test]
fn remove_is_a_single_remove() {
    let mut e = load("vdo_remove", APP);
    add(&mut e, "a", 0);
    add(&mut e, "b", 1);
    let prev = listing(&e); // [a, b]
    commit(&mut e, CallRequest::new("remove").arg("name", text("a")));
    let next = listing(&e); // [b]
    assert!(matches!(ops(&prev, &next).as_slice(), [PatchOp::Remove { .. }]));
}

#[test]
fn rekey_diffs_as_remove_of_old_and_insert_of_new_leaving_others_untouched() {
    // Assigning the key field is an atomic rekey (§5.4); the key-derived identity
    // changes, so between renders it as a remove of the old key and an insert of
    // the new one (see PatchOp::Rekey). The untouched sibling gets no op.
    let mut e = load("vdo_rekey", APP);
    add(&mut e, "a", 0);
    add(&mut e, "m", 0);
    let prev = listing(&e); // [a, m]  (equal prio, name tiebreak)
    commit(&mut e, CallRequest::new("rekey").arg("old", text("a")).arg("new", text("z")));
    let next = listing(&e); // [m, z]
    let ops = ops(&prev, &next);
    assert_eq!(ops.len(), 2, "exactly a remove and an insert, {ops:?}");
    assert!(ops.iter().any(|op| matches!(op, PatchOp::Remove { .. })), "{ops:?}");
    assert!(ops.iter().any(|op| matches!(op, PatchOp::Insert { .. })), "{ops:?}");
    assert!(
        !ops.iter().any(|op| matches!(op, PatchOp::Update { .. } | PatchOp::Move { .. })),
        "the sibling `m` is untouched, {ops:?}",
    );
}
