#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 live-view patch coherence over a SORTED view — the correctness oracle.
//!
//! SPEC.md §12.2 fixes the patch vocabulary a subscription delivers and its
//! coherence requirement:
//!
//! ```text
//! insert { $at, $id, $value }   remove { $id }   move { $id, $to }
//! update { $id, $value }        rekey  { $id, $key }
//! ```
//!
//! "`$at` and `$to` are zero-based positions in the current result. `update`
//! replaces the occurrence value while preserving identity. ... After applying
//! every operation, the client result MUST equal the authorized declared view at
//! the new frontier."
//!
//! `apply_patch` below is a FAITHFUL §12.2 client: it applies the ordered
//! [`ViewDelta`] op sequence one op at a time, each position interpreted in the
//! current (mid-application) result, never re-sorting. Every case asserts the
//! applied client result — occurrences, exposed values, AND order — equals
//! `view_at_head` of the sorted view. The expected order is externally deducible
//! from each view's `$sort`.
//!
//! Before the fix `ViewDelta` was the positionless `{ added, removed, changed }`
//! and these applied-order equalities failed (a front insert appended; a reorder
//! produced no move). The ordered §12.2 op vocabulary makes them hold.

mod support;

use std::collections::BTreeMap;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, PatchOp, Precision, RowId, SurfaceBinding, SurfaceHost, SurfaceRouter,
    SurfaceRouterBuilder, Value, ViewBinding, ViewDelta, ViewResult, ViewRow, VirtualClock,
};
use liasse_value::Integer;
use support::{add_task, apply_patch, call, host, store, text, NOW};

// The faithful §12.2 client that applies an ordered patch to the client's prior
// ordered rows — each `$at`/`$to` read in the working result as it stands, the
// client never re-sorting — is `support::apply_patch`, shared by every red_* test
// and backed by the one `liasse_wire::apply`.

/// The client-visible content of a result: each occurrence's identity and exposed
/// output fields, in order. This is what §12.2 requires the client result to
/// equal — the internal `$sort` tuple is not on the wire, so it is not compared.
fn visible(rows: &[ViewRow]) -> Vec<(RowId, BTreeMap<String, Value>)> {
    rows.iter()
        .map(|row| (row.id().clone(), row.fields().map(|(k, v)| (k.clone(), v.clone())).collect()))
        .collect()
}

/// Assert the faithfully-applied patch equals the authorized declared view at the
/// new frontier (§12.2), returning the ops for further shape checks.
fn assert_coherent(prev: &ViewResult, next: &ViewResult) -> Vec<PatchOp> {
    let delta = ViewDelta::between(Some(prev), next);
    let client = apply_patch(prev.rows(), &delta);
    assert_eq!(
        visible(&client),
        visible(next.rows()),
        "§12.2: after applying every operation the client result MUST equal the authorized \
         declared view at the new frontier (order included)",
    );
    match delta {
        ViewDelta::Patch(ops) => ops,
        ViewDelta::Init(_) => panic!("between(Some, _) is a patch, not an init"),
        ViewDelta::Scalar(_) => panic!("a sorted row view never yields a scalar delta"),
    }
}

// --- index host: `.tasks { id, title, $sort: [title] }` (title projected) ------

fn index(host: &SurfaceHost<MemoryStore>) -> ViewResult {
    host.engine().view_at_head("index").expect("view evaluates").expect("index declared")
}

#[test]
fn front_insert_places_the_new_row_at_position_zero() {
    let mut host = host();
    host.connect("c1");
    add_task(&mut host, "c1", "m");
    let prev = index(&host); // [m]
    add_task(&mut host, "c1", "a");
    let next = index(&host); // [a, m]

    let ops = assert_coherent(&prev, &next);
    assert!(
        matches!(ops.as_slice(), [PatchOp::Insert { at: 0, .. }]),
        "a front insert is `insert {{ at: 0 }}`, got {ops:?}",
    );
}

#[test]
fn middle_insert_places_the_new_row_at_its_sorted_position() {
    let mut host = host();
    host.connect("c1");
    add_task(&mut host, "c1", "a");
    add_task(&mut host, "c1", "z");
    let prev = index(&host); // [a, z]
    add_task(&mut host, "c1", "m");
    let next = index(&host); // [a, m, z]

    let ops = assert_coherent(&prev, &next);
    assert!(
        matches!(ops.as_slice(), [PatchOp::Insert { at: 1, .. }]),
        "a middle insert is `insert {{ at: 1 }}`, got {ops:?}",
    );
}

#[test]
fn front_to_back_move_via_sort_key_change_updates_and_moves() {
    // The sort key (`title`) is a projected field, so renaming both changes the
    // exposed value (update) and moves the occurrence.
    let mut host = host();
    host.connect("c1");
    add_task(&mut host, "c1", "m");
    let a_id = add_task(&mut host, "c1", "a");
    let prev = index(&host); // [a, m]

    let rename = call("public.tasks.rename", [("id", a_id), ("title", text("z"))]);
    assert!(host.call("c1", &rename).expect("rename").is_ok(), "rename commits");
    let next = index(&host); // [m, z]

    let ops = assert_coherent(&prev, &next);
    assert!(
        ops.iter().any(|op| matches!(op, PatchOp::Update { .. }))
            && ops.iter().any(|op| matches!(op, PatchOp::Move { .. })),
        "a projected-sort-key change both updates the value and moves the row, got {ops:?}",
    );
}

#[test]
fn remove_drops_the_departed_occurrence() {
    let mut host = host();
    host.connect("c1");
    let a_id = add_task(&mut host, "c1", "a");
    add_task(&mut host, "c1", "m");
    let prev = index(&host); // [a, m]

    assert!(host.call("c1", &call("public.tasks.remove", [("id", a_id)])).expect("remove").is_ok());
    let next = index(&host); // [m]

    let ops = assert_coherent(&prev, &next);
    assert!(matches!(ops.as_slice(), [PatchOp::Remove { .. }]), "a single removal, got {ops:?}");
}

#[test]
fn combined_commit_across_several_mutations_stays_coherent() {
    // A patch between two non-adjacent frontiers combines a remove, a value+move,
    // and an insert in one ordered sequence.
    let mut host = host();
    host.connect("c1");
    let b_id = add_task(&mut host, "c1", "b");
    let d_id = add_task(&mut host, "c1", "d");
    let prev = index(&host); // [b, d]

    add_task(&mut host, "c1", "a"); // -> [a, b, d]
    let rename = call("public.tasks.rename", [("id", d_id), ("title", text("c"))]);
    assert!(host.call("c1", &rename).expect("rename").is_ok()); // d-occurrence: title d->c
    assert!(host.call("c1", &call("public.tasks.remove", [("id", b_id)])).expect("remove").is_ok());
    let next = index(&host); // [a, c]

    let ops = assert_coherent(&prev, &next);
    assert!(ops.len() >= 2, "a combined commit yields several ordered ops, got {ops:?}");
}

// --- patch host: `.items { name, label, $sort: [prio, name] }` (prio not exposed)

const PATCH_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.patchcases@1.0.0"
  "$model": {
    "items": { "$key": "name", "name": "text", "label": "text", "prio": "int = 0" }
    "listing": { "$view": ".items { name, label, $sort: [prio, name] }" }
    "$mut": {
      "add": ".items + { name: @name, label: @label, prio: @prio }"
      "setprio": ".items[@name].prio = @prio"
      "rekey": ".items[@old].name = @new"
    }
    "$public": {
      "items": {
        "$view": ".listing"
        "$mut": { "add": ".add", "setprio": ".setprio", "rekey": ".rekey" }
      }
    }
  }
}"#;

fn patch_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = Engine::load(store("patchcases"), PATCH_APP, &mut clock).expect("patch app loads");
    let router = patch_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn patch_router(model: &liasse_model::Model) -> SurfaceRouter {
    let items = SurfaceBinding::new()
        .with_view(ViewBinding::new("listing"))
        .with_call(
            "add",
            CallBinding::root("add", ["name".to_owned(), "label".to_owned(), "prio".to_owned()]),
        )
        .with_call("setprio", CallBinding::root("setprio", ["name".to_owned(), "prio".to_owned()]))
        .with_call("rekey", CallBinding::root("rekey", ["old".to_owned(), "new".to_owned()]));
    SurfaceRouterBuilder::new()
        .public_surface("items", items)
        .build(model)
        .expect("router validates against the patchcases model")
}

fn listing(host: &SurfaceHost<MemoryStore>) -> ViewResult {
    host.engine().view_at_head("listing").expect("view evaluates").expect("listing declared")
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

fn add_item(host: &mut SurfaceHost<MemoryStore>, name: &str, label: &str, prio: i64) {
    let add =
        call("public.items.add", [("name", text(name)), ("label", text(label)), ("prio", int(prio))]);
    assert!(host.call("c1", &add).expect("add").is_ok(), "add {name} commits");
}

#[test]
fn pure_reorder_with_unchanged_fields_is_a_move() {
    // `prio` is the sort key but is NOT projected, so changing it moves the row
    // while its exposed value (`name`, `label`) is unchanged: a `move` with no
    // `update`. The old positionless delta could not express this at all.
    let mut host = patch_host();
    host.connect("c1");
    add_item(&mut host, "a", "A", 0);
    add_item(&mut host, "b", "B", 1);
    let prev = listing(&host); // [a, b]  (prio 0 < 1)

    let bump = call("public.items.setprio", [("name", text("a")), ("prio", int(5))]);
    assert!(host.call("c1", &bump).expect("setprio").is_ok(), "setprio commits");
    let next = listing(&host); // [b, a]  (a now prio 5)

    let ops = assert_coherent(&prev, &next);
    assert!(
        ops.iter().any(|op| matches!(op, PatchOp::Move { .. }))
            && !ops.iter().any(|op| matches!(op, PatchOp::Update { .. })),
        "a pure reorder is a `move` with no `update` (exposed value unchanged), got {ops:?}",
    );
}

#[test]
fn rekey_is_a_remove_and_insert_that_reaches_the_authorized_order() {
    // Assigning the key field `name` performs an atomic rekey (§5.4). The
    // occurrence's key-derived identity changes, so between renders it as a remove
    // of the old key and an insert of the new one — still reaching the authorized
    // ordered view (§12.2). Emitting a `rekey` op needs occurrence continuity the
    // key-derived result does not carry (documented seam).
    let mut host = patch_host();
    host.connect("c1");
    add_item(&mut host, "a", "A", 0);
    add_item(&mut host, "m", "M", 0);
    let prev = listing(&host); // [a, m]  (equal prio, name tiebreak)

    let rk = call("public.items.rekey", [("old", text("a")), ("new", text("z"))]);
    assert!(host.call("c1", &rk).expect("rekey").is_ok(), "rekey commits");
    let next = listing(&host); // [m, z]  (a -> z sorts after m)

    let ops = assert_coherent(&prev, &next);
    assert!(
        ops.iter().any(|op| matches!(op, PatchOp::Remove { .. }))
            && ops.iter().any(|op| matches!(op, PatchOp::Insert { .. })),
        "a key change diffs as remove + insert, got {ops:?}",
    );
}
