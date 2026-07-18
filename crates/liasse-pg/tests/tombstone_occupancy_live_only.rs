//! Occupancy stays LIVE-only after the `by_id` map became the *structural* node
//! index.
//!
//! The tombstone fix keeps a deleted non-leaf ancestor as a structural node so a
//! nested row can still resolve its parent, and the follow-on fix makes the write
//! path's `by_id` map hold EVERY node (live rows AND tombstones) so that parent
//! resolution finds a tombstoned ancestor
//! ([`redteam_insert_under_tombstone_divergence`]). That structural index must NOT
//! leak into occupancy: a tombstoned address is *absent* for the purpose of
//! insert/delete/rekey admission. Occupancy is decided against the LIVE-row index
//! (`current`), never `by_id`, so:
//!
//!   - insert onto a LIVE row still errors `Conflict` (the address is occupied);
//!   - delete of a tombstoned (or never-existed) address still errors `NotFound`
//!     (a double-delete does not succeed just because the tombstone node lingers);
//!   - insert onto a TOMBSTONED address succeeds — it REVIVES the row with a fresh
//!     incarnation (D.1), exactly as the reference `MemoryStore` re-admits it.
//!
//! These are the contract's own preconditions (`Transition::insert` errors
//! `Conflict` only on occupancy; `delete` errors `NotFound` only when absent), and
//! they are externally deducible from the contract independent of any backend's
//! answer. The pg backend is gated to match the `MemoryStore` oracle op-for-op.
//!
//! Like the rest of the suite it resolves the DSN through [`support`] and drops its
//! throwaway schema through a [`support::SchemaGuard`] even on a panic.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress, StoreError, StoreFactory,
    StoredRow, Transition,
};
use liasse_value::{Integer, Text, Value};

fn org(o: i64) -> RowAddress {
    RowAddress::root(AddressStep::new(
        NameSegment::new("orgs"),
        KeyValue::single(Value::Int(Integer::from(o))),
    ))
}
fn text(payload: &str) -> Value {
    Value::Text(Text::new(payload))
}

/// The occupancy verdicts a store returns as `/orgs/1` moves live -> tombstoned ->
/// revived. Booleans/None are directly comparable across backends; the error kinds
/// are compared by pattern so the two backends' verdicts can be asserted equal.
struct Occupancy {
    /// Staging a second insert over the LIVE `/orgs/1`.
    reinsert_live: Result<(), StoreError>,
    /// Row read after `/orgs/1` is deleted (tombstoned): must be `None`.
    row_after_delete: Option<StoredRow>,
    /// Staging a second delete of the now-tombstoned `/orgs/1`.
    double_delete: Result<(), StoreError>,
    /// Staging an insert onto the tombstoned `/orgs/1` (a revive).
    revive: Result<(), StoreError>,
    /// Row read after the revive commits: must be present again.
    row_after_revive: Option<StoredRow>,
}

/// Drive the identical occupancy workload on `store`, capturing each admission
/// verdict rather than unwrapping, so the two backends can be compared directly.
fn occupancy<S: InstanceStore>(store: &mut S) -> Occupancy {
    // Seed a live row.
    let mut txn = store.begin();
    let first = txn.insert(org(1), text("org-1")).expect("insert /orgs/1");
    txn.commit().expect("commit insert /orgs/1");

    // Insert onto the LIVE row: occupancy must reject it.
    let mut txn = store.begin();
    let reinsert_live = txn.insert(org(1), text("dup")).map(|_| ());
    drop(txn); // abort — the staged insert never commits

    // Delete it: the pg node is tombstoned, the live row is gone.
    let mut txn = store.begin();
    txn.delete(&org(1)).expect("delete /orgs/1");
    txn.commit().expect("commit delete /orgs/1");
    let row_after_delete = store.row(&org(1)).expect("row /orgs/1 after delete");

    // Delete again: the tombstone lingers structurally but the address is ABSENT
    // for occupancy, so a double-delete must error `NotFound`.
    let mut txn = store.begin();
    let double_delete = txn.delete(&org(1));
    drop(txn);

    // Insert onto the tombstoned address: occupancy sees it as free, so this
    // REVIVES the row (fresh incarnation) rather than conflicting.
    let mut txn = store.begin();
    let revive = txn.insert(org(1), text("org-1-again")).map(|_| ());
    if revive.is_ok() {
        txn.commit().expect("commit revive /orgs/1");
    } else {
        drop(txn); // abort — nothing to commit
    }
    let row_after_revive = store.row(&org(1)).expect("row /orgs/1 after revive");

    // The revive must mint a FRESH incarnation, not resurrect the pre-delete one.
    if let Some(revived) = &row_after_revive {
        assert_ne!(
            revived.incarnation(),
            &first,
            "a revive allocates a fresh incarnation (D.1), not the pre-delete token"
        );
    }

    Occupancy { reinsert_live, row_after_delete, double_delete, revive, row_after_revive }
}

#[test]
fn occupancy_is_live_only_across_a_tombstone() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("tombstoneoccupancy");
    let instance = InstanceId::new("tombstone-occupancy-live-only");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    let mem = occupancy(&mut memory);

    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");
    let pg = occupancy(&mut pg);

    // Externally deducible from the contract, independent of any backend:
    // insert-onto-live conflicts, double-delete is NotFound, revive succeeds.
    assert!(
        matches!(mem.reinsert_live, Err(StoreError::Conflict { .. })),
        "insert onto a live row conflicts (oracle), got {:?}",
        mem.reinsert_live
    );
    assert!(
        matches!(mem.double_delete, Err(StoreError::NotFound { .. })),
        "double-delete of an absent row is NotFound (oracle), got {:?}",
        mem.double_delete
    );
    assert!(mem.revive.is_ok(), "revive onto a tombstone succeeds (oracle), got {:?}", mem.revive);
    assert!(mem.row_after_delete.is_none(), "deleted row reads absent (oracle)");
    assert!(mem.row_after_revive.is_some(), "revived row reads present (oracle)");

    // 0-divergence: the pg backend must return the identical occupancy verdicts —
    // the structural `by_id` (which now holds the tombstone node) must NOT make the
    // tombstoned address read as occupied.
    assert!(
        matches!(pg.reinsert_live, Err(StoreError::Conflict { .. })),
        "pg must Conflict on insert-onto-live like the oracle, got {:?}",
        pg.reinsert_live
    );
    assert!(
        matches!(pg.double_delete, Err(StoreError::NotFound { .. })),
        "pg must return NotFound on a double-delete like the oracle (a lingering tombstone \
         node must not make the address occupied), got {:?}",
        pg.double_delete
    );
    assert!(pg.revive.is_ok(), "pg must admit a revive onto a tombstone like the oracle, got {:?}", pg.revive);
    assert_eq!(
        pg.row_after_delete, mem.row_after_delete,
        "pg row-after-delete must match the oracle (absent)"
    );
    assert_eq!(
        pg.row_after_revive, mem.row_after_revive,
        "pg row-after-revive must match the oracle (present, fresh incarnation)"
    );
}
