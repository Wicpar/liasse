//! RED TEAM — the node-tree cascade-delete narrowing is OBSERVABLE across a reopen.
//!
//! The node-adjacency backend (`node_write::apply`, the `Delete` arm) cascade-deletes
//! a deleted row's whole descendant subtree from the durable `nodes` table with a
//! recursive CTE, justified as an "accepted narrowing": the runtime, on a *top-level*
//! row drop, emits only the top-level `Delete` and leaves the nested rows as "logical
//! orphans (unreachable)", so — the argument goes — dropping them physically is
//! unobservable.
//!
//! That claim is FALSE. Two facts collide:
//!
//! 1. The runtime really does emit only the top-level `Delete` for a top-level drop.
//!    `interp::apply_deletion` (the §21.1 planner path) removes ONLY the top-level row
//!    address from the working set; it never calls `remove_subtree`, so a nested
//!    descendant is left in committed state (`state::diff` emits no `Delete` for it).
//!    The in-memory reference store — the spec oracle — therefore RETAINS the
//!    descendant row at its address after the parent is deleted.
//! 2. The pg backend cannot represent that state. Its durable node tree cascade-drops
//!    the descendant (a child node cannot dangle off a deleted parent), while its
//!    *live projection* (`projection::apply_op`, the `Delete` arm) removes ONLY the
//!    addressed row from `current`, leaving the descendant in the in-memory read model.
//!
//! So the pg live projection and the pg durable tree DISAGREE about the descendant the
//! instant the parent is deleted — contradicting the projection's own documented
//! invariant that it is "equal to durable state by construction". A reopen, which
//! rebuilds the read model purely from the durable tables, exposes the disagreement:
//! the orphan is present before the reopen and GONE after it.
//!
//! The divergence is observable through the ordinary store contract:
//!
//! - `MemoryStore.scan(/orgs/1/teams)`      => [team(1,10)]   (oracle retains the orphan)
//! - `PgStore.scan(/orgs/1/teams)` (live)   => [team(1,10)]   (projection retains it)
//! - `PgStore.scan(/orgs/1/teams)` (reopen) => []             (durable tree cascaded it)
//!
//! Cited spec / contract:
//!
//! - §5.4 "Nested collections therefore have row identity plus ancestor identity" — a
//!   nested row is a distinct row whose deletion is a distinct op; the store must not
//!   invent an implicit cascade the committed op stream never expressed.
//! - §21.1 deferred-delete: the committed transition deletes exactly the planned rows;
//!   a top-level drop's plan names only the top-level row (its nested rows carry no
//!   inbound CORE refs), so the nested row is not in the delete set.
//! - The store durability contract (crate test `reopen_rebuilds_state_from_durable
//!   _tables`; §22.7/§19.2 "a reopen rebuilds an identical projection"): reopening
//!   MUST reproduce the exact observable state the live store held. Here it does not.
//! - The overarching gate: the pg backend must be 0-divergence vs `MemoryStore`.
//!
//! Like the rest of the suite it resolves the DSN through [`support`] and drops its
//! throwaway schema through a [`support::SchemaGuard`] even on a panic.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress,
    StoreFactory, StoredRow, Transition,
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

/// The identical op sequence both backends run — exactly the op stream the runtime
/// emits for a top-level row drop that has a nested child: insert the parent and a
/// nested child, then delete ONLY the top-level parent. No `Delete` is ever issued
/// for the nested child (the runtime leaves it a logical orphan; §21.1 planner path).
fn apply_workload<S: InstanceStore>(store: &mut S) {
    let mut txn = store.begin();
    txn.insert(org(1), text("org-1")).expect("insert org 1");
    txn.insert(team(1, 10), text("team-1-10")).expect("insert nested team 1/10");
    txn.commit().expect("commit inserts");

    // Top-level drop of org 1. The nested team 1/10 is NOT deleted — the committed
    // transition carries a single `Delete(/orgs/1)`, leaving team 1/10 an orphan.
    let mut txn = store.begin();
    txn.delete(&org(1)).expect("delete org 1 (top-level drop)");
    txn.commit().expect("commit delete");
}

/// The direct rows of `/orgs/1/teams`, in Annex B order — the backend-agnostic
/// observable both stores are compared on. `RowAddress` and `StoredRow` both derive
/// structural equality, so two backends' results compare directly.
fn nested_teams<S: InstanceStore>(store: &S) -> Vec<(RowAddress, StoredRow)> {
    let teams = CollectionPath::nested(org(1).steps().cloned(), NameSegment::new("teams"));
    let mut rows: Vec<(RowAddress, StoredRow)> = store.scan(&teams).expect("scan /orgs/1/teams");
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

#[test]
fn top_level_delete_orphan_diverges_on_reopen() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("orphanreopen");
    let instance = InstanceId::new("cascade-delete-orphan-reopen");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    // The oracle and the pg store run the identical workload, so their opaque
    // `row-N` incarnations line up op-for-op.
    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    apply_workload(&mut memory);

    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");
    apply_workload(&mut pg);

    let memory_teams = nested_teams(&memory);
    let pg_live_teams = nested_teams(&pg);

    // The oracle retains the orphaned nested row after the parent's top-level drop:
    // exactly one row, team 1/10, at the address it was inserted at.
    assert_eq!(
        memory_teams.len(),
        1,
        "the in-memory oracle must retain the nested orphan row after a top-level drop, got {memory_teams:?}"
    );
    assert_eq!(memory_teams[0].0, team(1, 10), "the retained orphan is /orgs/1/teams/10");
    assert_eq!(memory_teams[0].1.value(), &text("team-1-10"), "with its original value");

    // BEFORE the reopen the pg live projection agrees with the oracle — it, too,
    // retained the orphan (its `apply_op` Delete removed only the top-level address).
    // This is what makes the narrowing look unobservable while the store stays open.
    assert_eq!(
        pg_live_teams, memory_teams,
        "sanity: pre-reopen, the pg live projection matches the oracle (both retain the orphan)"
    );

    // Reopen from the durable tables — the path a process restart takes. The read
    // model is rebuilt purely from the node tree.
    let reopened = pg_factory.reopen(instance.clone()).expect("reopen pg store");
    let pg_reopened_teams = nested_teams(&reopened);

    // The store durability contract: a reopen rebuilds the IDENTICAL observable state
    // the live store held. It does not — the orphan the live projection returned is
    // gone, because the durable node tree cascade-deleted the subtree on the parent's
    // delete.
    assert_eq!(
        pg_reopened_teams, pg_live_teams,
        "reopen-durability (crate `reopen_rebuilds_state_from_durable_tables`, \
         §22.7/§19.2): a reopen changed observable state — the nested orphan present \
         in the live projection vanished after reopening from the durable node tree"
    );

    // The overarching gate: 0-divergence vs the MemoryStore oracle. After a reopen the
    // pg store loses a row the oracle keeps.
    assert_eq!(
        pg_reopened_teams, memory_teams,
        "0-divergence vs MemoryStore (§5.4/§21.1): after a top-level drop leaves a \
         nested row as a logical orphan, the pg node tree cascade-deletes it while the \
         oracle retains it — observable via scan(/orgs/1/teams) once the store reopens. \
         oracle={memory_teams:?} pg_reopened={pg_reopened_teams:?}"
    );
    // `_guard` drops the throwaway schema on scope exit (and on a panic).
}
