//! RED TEAM — a nested insert / rekey whose ancestor address was NEVER a node.
//!
//! The store contract is "semantics-free: it stores, orders, and retrieves ...
//! none of it validates types, refs, checks, or authorization" (contract.rs, §23).
//! `Transition::insert` pins exactly one precondition — `Conflict` if the address
//! already holds a row — and `rekey` pins two — `NotFound` if `from` is absent,
//! `Conflict` if `to` is occupied. Neither says the target's *ancestor* row must
//! exist. The in-memory reference honors that literally: its state is a flat
//! `BTreeMap<RowAddress, StoredRow>`, so a nested row at `/orgs/1/teams/10` is
//! admitted whether or not `/orgs/1` ever existed.
//!
//! The PostgreSQL backend cannot: the `nodes` adjacency tree resolves a row's
//! parent by surrogate id, and a parent that was never inserted has no node, so the
//! write path returns `StoreError::Corruption` ("no node for row address ...")
//! rather than admitting the row. The two backends therefore disagree on the exact
//! same contract call — one commits, the other errors.
//!
//! This is a latent divergence: the runtime layered above the store always inserts
//! an ancestor row before any nested row (§5.4 nested rows carry ancestor
//! identity), so a *runtime-produced* op stream never triggers it. But the store
//! contract is the conformance boundary the identical battery runs against, and on
//! that boundary the backends are not interchangeable here. Reported as a
//! divergence, not silently skip-listed.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress, StoreFactory, Transition,
};
use liasse_value::{Integer, Text, Value};

use support::SchemaGuard;

fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}
fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// `/orgs/{o}` — a top-level row.
fn org(o: i64) -> RowAddress {
    RowAddress::root(AddressStep::new(NameSegment::new("orgs"), KeyValue::single(int(o))))
}

/// `/orgs/{o}/teams/{t}` — a nested row two levels down.
fn team(o: i64, t: i64) -> RowAddress {
    org(o).child(AddressStep::new(NameSegment::new("teams"), KeyValue::single(int(t))))
}

/// Insert ONE nested row whose top-level ancestor `/orgs/1` was never inserted,
/// commit, and report the outcome as `Ok(())` (committed) or `Err` (rejected).
fn insert_orphan_nested<S: InstanceStore>(store: &mut S) -> Result<(), String> {
    let mut txn = store.begin();
    // The single staged write: a nested row with no ancestor row anywhere.
    txn.insert(team(1, 10), text("orphan"))
        .map_err(|e| format!("stage insert failed: {e:?}"))?;
    txn.commit().map_err(|e| format!("commit failed: {e:?}"))?;
    Ok(())
}

#[test]
fn nested_insert_without_ancestor_diverges_memory_vs_pg() {
    // Reference: the in-memory store admits the orphan nested row.
    let mut mem = MemoryStoreFactory::new().create(InstanceId::new("orphan-nested")).expect("mem");
    let mem_outcome = insert_orphan_nested(&mut mem);

    // Backend under test: the PostgreSQL store.
    let handle = support::acquire();
    let mut factory = handle.factory("orphan_nested");
    let instance = InstanceId::new("orphan-nested");
    let _schema = SchemaGuard::new(&factory, instance.clone());
    let mut pg = factory.create(instance.clone()).expect("pg create");
    let pg_outcome = insert_orphan_nested(&mut pg);

    // The store contract is semantics-free and pins no ancestor precondition, so
    // both backends MUST agree on whether this single contract call is admitted.
    assert_eq!(
        mem_outcome.is_ok(),
        pg_outcome.is_ok(),
        "store-contract divergence on a nested insert with no ancestor row:\n  \
         MemoryStore => {mem_outcome:?}\n  PgStore     => {pg_outcome:?}"
    );

    // If both admitted it, the observable state must match too (the orphan row is
    // readable and scannable from each).
    if mem_outcome.is_ok() && pg_outcome.is_ok() {
        assert_eq!(
            mem.row(&team(1, 10)).expect("mem row").map(|r| r.value().clone()),
            pg.row(&team(1, 10)).expect("pg row").map(|r| r.value().clone()),
            "the admitted orphan row must read back equal on both backends"
        );
    }
}
