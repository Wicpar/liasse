//! RED TEAM — the tombstone model cannot place a NEW nested row under a
//! tombstoned (absent) ancestor, so an insert the reference store admits is
//! REJECTED by the pg backend.
//!
//! The just-landed tombstone model (commit a3870f1) keeps a deleted non-leaf
//! ancestor as a structural-only node "so its descendant rows (logical orphans,
//! §5.4) stay addressable". Reads honor that: `node_load` walks the parent chain
//! *through* tombstones, and the reopen test reconstructs an orphan's address
//! across a tombstoned ancestor. But the WRITE path does not: it resolves a
//! nested insert's parent id against the projection's `by_id` map, and that map
//! is built from — and maintained as — LIVE rows only (a `Delete` calls
//! `by_id.remove(address)`; `node_load` inserts only live nodes into it). So the
//! instant an ancestor is tombstoned, its id is gone from the write path's
//! resolver, and any later attempt to place a child under that ancestor fails
//! `resolve_parent` -> `resolve_id` with an internal `Corruption` ("no node for
//! row address …") — even though the tombstone node exists for exactly this
//! "stay addressable" purpose.
//!
//! This is an asymmetry between the two halves of the same backend:
//!
//!   - node_load.rs (read):  parent-walk traverses tombstones  -> orphans READABLE
//!   - node_write.rs (write): parent resolves via live-only by_id -> orphans NOT
//!                            EXTENDABLE  (resolve_id errors on a tombstoned parent)
//!
//! The storage contract is semantics-free and admits a row at ANY unoccupied
//! address. `Transition::insert` errors only on OCCUPANCY ("Errors Conflict if
//! the address already holds a row"); it states NO ancestor-existence
//! precondition, and staging is occupancy-only in BOTH backends
//! (`staging.rs` / `transition.rs`). §5.4 gives a nested row "row identity plus
//! ancestor identity": it is a distinct row addressable by its full path,
//! independent of whether its ancestor currently holds a value (that is what a
//! "logical orphan" is). The reference `MemoryStore` — the spec oracle — admits
//! the insert and returns the new row from a scan. The pg backend rejects the
//! commit outright.
//!
//! Observable through the ordinary store contract, on the IDENTICAL op sequence:
//!
//!   insert /orgs/1 ; delete /orgs/1 ; insert /orgs/1/teams/10
//!
//!   - MemoryStore.commit(insert /orgs/1/teams/10) => Ok(Committed)   (oracle admits it)
//!   - PgStore.commit(insert /orgs/1/teams/10)     => Err(Corruption) (write path can't
//!                                                     resolve the tombstoned parent)
//!   - MemoryStore.scan(/orgs/1/teams)             => [team 1/10]
//!   - PgStore.scan(/orgs/1/teams)                 => []              (nothing committed)
//!
//! Cited spec / contract:
//!
//! - §5.4 "Nested collections therefore have row identity plus ancestor identity"
//!   — a nested row is a distinct, fully-addressable row; its existence does not
//!   require its ancestor to currently be a live row (a logical orphan is exactly
//!   an addressable descendant of an absent ancestor).
//! - The `Transition::insert` contract (`crates/liasse-store/src/contract.rs`):
//!   the ONLY documented precondition is occupancy ("Errors `Conflict` if the
//!   address already holds a row"). An absent ancestor is not a rejection cause.
//! - The overarching gate: the pg backend must be 0-divergence vs `MemoryStore`.
//!   The store's own doc: the projection "is equal to durable state by
//!   construction" and a reopen "rebuilds an identical projection" (§22.7/§19.2)
//!   — but here the two backends already disagree at commit admission.
//!
//! Like the rest of the suite it resolves the DSN through [`support`] and drops
//! its throwaway schema through a [`support::SchemaGuard`] even on a panic.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::doc_overindented_list_items
)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, CommitOutcome, InstanceStore, KeyValue, MemoryStoreFactory,
    RowAddress, StoreError, StoreFactory, StoredRow, Transition,
};
use liasse_value::{Integer, Text, Value};

/// One address level `name/<int key>`.
fn step(name: &str, key: i64) -> AddressStep {
    AddressStep::new(NameSegment::new(name), KeyValue::single(Value::Int(Integer::from(key))))
}
fn org(o: i64) -> RowAddress {
    RowAddress::root(step("orgs", o))
}
fn team(o: i64, t: i64) -> RowAddress {
    org(o).child(step("teams", t))
}
fn text(payload: &str) -> Value {
    Value::Text(Text::new(payload))
}

/// The identical op sequence both backends run, in three separate commits:
///
///   1. insert /orgs/1                 (a plain top-level row)
///   2. delete /orgs/1                 (tombstones it in the pg node tree; `by_id`
///                                       drops the address — no children yet)
///   3. insert /orgs/1/teams/10        (a NEW nested row under the now-absent /orgs/1)
///
/// The first two commits succeed on both backends. The third is the divergence:
/// its `commit()` outcome is RETURNED (not `expect`ed) so the test can compare the
/// two backends' admission verdicts directly instead of panicking on the pg error.
fn seed_then_orphan_insert<S: InstanceStore>(store: &mut S) -> Result<CommitOutcome, StoreError> {
    let mut txn = store.begin();
    txn.insert(org(1), text("org-1")).expect("insert /orgs/1");
    txn.commit().expect("commit insert /orgs/1");

    let mut txn = store.begin();
    txn.delete(&org(1)).expect("delete /orgs/1");
    txn.commit().expect("commit delete /orgs/1");

    // Stage a nested row under the absent /orgs/1. Staging is occupancy-only in
    // BOTH backends, so this `insert` call succeeds on both (the address is free);
    // the ancestor's absence only bites the pg backend at commit time.
    let mut txn = store.begin();
    txn.insert(team(1, 10), text("team-1-10"))
        .expect("stage nested insert /orgs/1/teams/10 under the absent /orgs/1");
    txn.commit()
}

/// The direct rows of `/orgs/1/teams`, in Annex B order — the backend-agnostic
/// observable both stores are compared on.
fn nested_teams<S: InstanceStore>(store: &S) -> Vec<(RowAddress, StoredRow)> {
    let teams = CollectionPath::nested(org(1).steps().cloned(), NameSegment::new("teams"));
    let mut rows: Vec<(RowAddress, StoredRow)> = store.scan(&teams).expect("scan /orgs/1/teams");
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

#[test]
fn nested_insert_under_tombstoned_ancestor_diverges() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("tombstonechild");
    let instance = InstanceId::new("insert-under-tombstone-divergence");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    // The oracle and the pg store run the identical workload, so their opaque
    // `row-N` incarnations line up op-for-op.
    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    let memory_outcome = seed_then_orphan_insert(&mut memory);

    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");
    let pg_outcome = seed_then_orphan_insert(&mut pg);

    // The oracle admits the nested-orphan insert: `insert`'s only precondition is
    // occupancy, and /orgs/1/teams/10 is unoccupied. Externally deducible from the
    // contract, independent of any backend's answer.
    assert!(
        matches!(memory_outcome, Ok(CommitOutcome::Committed(_))),
        "the MemoryStore oracle must admit a nested row under an absent ancestor \
         (insert is occupancy-only per §5.4 + the Transition::insert contract), got {memory_outcome:?}"
    );

    // THE DIVERGENCE. The pg backend must match the oracle's admission verdict.
    // It does not: its write path resolves the nested insert's parent id against a
    // LIVE-ONLY `by_id` map, and /orgs/1 was tombstoned (dropped from `by_id`), so
    // `resolve_parent` -> `resolve_id` errors and the commit is rejected — even
    // though the tombstone node exists precisely to keep descendants addressable.
    assert!(
        matches!(pg_outcome, Ok(CommitOutcome::Committed(_))),
        "0-divergence vs MemoryStore (§5.4 / Transition::insert contract): the oracle \
         admitted the nested insert /orgs/1/teams/10 under the absent /orgs/1, but the pg \
         backend REJECTED the commit. Root cause: node_write::place -> resolve_parent -> \
         resolve_id looks the tombstoned ancestor up in the live-only projection `by_id` \
         (projection.rs `apply_node_id` Delete removes it; node_load only loads live nodes), \
         so the write path cannot place a child under a tombstone the read path can still \
         walk through. memory={memory_outcome:?} pg={pg_outcome:?}"
    );

    // Read-level corollary (only reached once the commit divergence is fixed): the
    // same op stream must leave both backends showing the orphan row, live and after
    // a reopen from the durable node tree.
    let memory_teams = nested_teams(&memory);
    assert_eq!(
        memory_teams.len(),
        1,
        "the oracle retains the nested orphan row it admitted, got {memory_teams:?}"
    );

    let pg_live_teams = nested_teams(&pg);
    assert_eq!(
        pg_live_teams, memory_teams,
        "live-projection divergence: pg scan(/orgs/1/teams) must match the oracle. \
         oracle={memory_teams:?} pg_live={pg_live_teams:?}"
    );

    let reopened = pg_factory.reopen(instance.clone()).expect("reopen pg store");
    let pg_reopened_teams = nested_teams(&reopened);
    assert_eq!(
        pg_reopened_teams, memory_teams,
        "reopen-durability (§22.7/§19.2): a reopen from the durable node tree must reproduce \
         the oracle's observable state. oracle={memory_teams:?} pg_reopened={pg_reopened_teams:?}"
    );
    // `_guard` drops the throwaway schema on scope exit (and on a panic).
}
