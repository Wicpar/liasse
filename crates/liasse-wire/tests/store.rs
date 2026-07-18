#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! [`WireStore`] lifecycle: a client replica folds the downstream frames of one
//! subscription into its retained result and frontier. The expected state after
//! each frame is computed from §12.2 by hand (init sets the rows; a patch advances
//! them; a frontier-only patch moves only the frontier; close/reset terminate),
//! never from the store's own output.

use std::collections::BTreeSet;

use liasse_wire::{
    CloseReason, Ft, Occ, PatchOp, ResetReason, StoreError, WireRow, WireStore, serde_json,
};
use serde_json::json;

fn row(id: &str, n: i64) -> WireRow {
    WireRow::new(Occ::new(id), json!(n))
}

fn ids(rows: &[WireRow]) -> Vec<String> {
    rows.iter().map(|r| r.occ().as_str().to_owned()).collect()
}

fn occ_set(names: &[&str]) -> BTreeSet<Occ> {
    names.iter().map(|n| Occ::new(*n)).collect()
}

#[test]
fn a_row_subscription_lives_through_init_patches_close_then_reset() {
    let mut store = WireStore::new();
    assert!(store.is_live(), "a fresh store is live but uninitialized");
    assert_eq!(store.frontier(), None);
    assert!(store.rows().is_empty());

    // init at f0.
    store.init(vec![row("a", 1), row("b", 2)], Ft::new("f0")).expect("init");
    assert_eq!(ids(store.rows()), ["a", "b"]);
    assert_eq!(store.frontier(), Some(&Ft::new("f0")));
    assert_eq!(store.known_occ(), occ_set(&["a", "b"]));

    // patch at f1: insert c at the end.
    store
        .patch(&[PatchOp::Insert { at: 2, occ: Occ::new("c"), value: json!(3) }], Ft::new("f1"))
        .expect("patch");
    assert_eq!(ids(store.rows()), ["a", "b", "c"]);
    assert_eq!(store.frontier(), Some(&Ft::new("f1")));
    assert_eq!(store.known_occ(), occ_set(&["a", "b", "c"]));

    // patch at f2: remove a.
    store.patch(&[PatchOp::Remove { occ: Occ::new("a") }], Ft::new("f2")).expect("patch");
    assert_eq!(ids(store.rows()), ["b", "c"]);

    // frontier-only patch at f3: rows unchanged, frontier advances.
    store.patch(&[], Ft::new("f3")).expect("empty patch");
    assert_eq!(ids(store.rows()), ["b", "c"]);
    assert_eq!(store.frontier(), Some(&Ft::new("f3")));

    // A bare frontier advance moves only the frontier.
    store.advance_frontier(Ft::new("f4")).expect("frontier ping");
    assert_eq!(store.frontier(), Some(&Ft::new("f4")));
    assert_eq!(ids(store.rows()), ["b", "c"]);

    // close terminates; further frames are rejected.
    store.close(CloseReason::Unauthorized);
    assert!(!store.is_live());
    assert_eq!(store.close_reason(), Some(CloseReason::Unauthorized));
    assert!(store.rows().is_empty(), "a closed store exposes no rows");
    assert_eq!(store.patch(&[], Ft::new("f5")), Err(StoreError::NotLive));

    // reset is terminal too.
    store.reset(ResetReason::UnknownConnection);
    assert_eq!(store.reset_reason(), Some(ResetReason::UnknownConnection));
    assert!(!store.is_live());
}

#[test]
fn a_rejected_patch_leaves_the_store_unchanged() {
    let mut store = WireStore::new();
    store.init(vec![row("a", 1)], Ft::new("f0")).expect("init");
    let err = store.patch(&[PatchOp::Remove { occ: Occ::new("x") }], Ft::new("f1")).unwrap_err();
    assert!(matches!(err, StoreError::Apply(_)));
    assert_eq!(ids(store.rows()), ["a"], "a failed patch does not mutate the rows");
    assert_eq!(store.frontier(), Some(&Ft::new("f0")), "nor the frontier");
}

#[test]
fn a_scalar_subscription_holds_its_value_and_refuses_row_patches() {
    let mut store = WireStore::new();
    store.scalar(json!(41), Ft::new("f0")).expect("scalar");
    assert_eq!(store.scalar_value(), Some(&json!(41)));
    assert!(store.rows().is_empty());

    store.scalar(json!(42), Ft::new("f1")).expect("scalar update");
    assert_eq!(store.scalar_value(), Some(&json!(42)));
    assert_eq!(store.frontier(), Some(&Ft::new("f1")));

    assert_eq!(store.patch(&[], Ft::new("f2")), Err(StoreError::ShapeMismatch));
}

#[test]
fn a_patch_before_init_is_rejected() {
    let mut store = WireStore::new();
    assert_eq!(store.patch(&[], Ft::new("f0")), Err(StoreError::NotInitialized));
}

#[test]
fn known_occ_is_empty_off_a_row_stream() {
    let store = WireStore::new();
    assert!(store.known_occ().is_empty());
}
