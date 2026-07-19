//! RED TEAM — durable burn-on-allocate incarnation (§6.3, D.1) checked for a
//! memory-vs-pg divergence across ABORTS, and for the durable no-reuse guarantee
//! across a REOPEN (SPEC-ISSUES item 32: a backend disagreement is always a fix).
//!
//! `PgStore::alloc_incarnation` is an AUTOCOMMIT `UPDATE instance_meta SET
//! next_incarnation = next_incarnation + 1 … RETURNING next_incarnation - 1`, so a
//! token is burned the instant it is handed out — whether or not the staging that
//! requested it later commits. The reference `MemoryStore::alloc_incarnation`
//! mutates its in-process counter through the transition's exclusive `&mut store`,
//! so an abort ALSO leaves the counter advanced. The two must therefore hand out
//! the IDENTICAL `row-N` sequence op-for-op even when transitions abort between
//! commits — an opaque-token identity the runtime relies on to distinguish row
//! generations (D.1). Attacked here:
//!
//!   * an abort of a multi-insert transition, then a committed insert: the burned
//!     tokens are NOT reused, and both backends agree on every returned token;
//!   * a rekey/update reuse NO fresh token (they carry the source incarnation), so
//!     the counter only advances on `insert`;
//!   * the pg counter is DURABLE: after a reopen, a fresh insert continues past
//!     every previously burned token, never recycling one (the reopen-faithfulness
//!     §6.3 claims over the old commit-time persist).
//!
//! Resolves the DSN through [`support`] and drops its throwaway schema through a
//! [`support::SchemaGuard`] even on a panic.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use liasse_ident::{InstanceId, NameSegment, RowIncarnation};
use liasse_store::{
    AddressStep, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress, StoreFactory, Transition,
};
use liasse_value::{Integer, Text, Value};

fn item(k: i64) -> RowAddress {
    RowAddress::root(AddressStep::new(
        NameSegment::new("items"),
        KeyValue::single(Value::Int(Integer::from(k))),
    ))
}
fn text(payload: &str) -> Value {
    Value::Text(Text::new(payload))
}

/// Run the identical alloc-heavy, abort-interleaved script against a store,
/// returning every `row-N` token `insert` handed out, in order. Committed rows are
/// left behind; aborted rows are discarded but their tokens stay burned.
fn burn_script<S: InstanceStore>(store: &mut S) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut record = |inc: RowIncarnation| tokens.push(inc.as_str().to_owned());

    // txn A: two inserts, then ABORT — both tokens burned.
    let mut a = store.begin();
    record(a.insert(item(1), text("a1")).expect("A insert 1"));
    record(a.insert(item(2), text("a2")).expect("A insert 2"));
    a.abort();

    // txn B: one insert, COMMIT.
    let mut b = store.begin();
    record(b.insert(item(3), text("b3")).expect("B insert 3"));
    b.commit().expect("B commit");

    // txn C: two inserts, an UPDATE (reuses, no new token) and a DELETE, then ABORT.
    let mut c = store.begin();
    record(c.insert(item(4), text("c4")).expect("C insert 4"));
    record(c.insert(item(5), text("c5")).expect("C insert 5"));
    c.update(&item(3), text("b3-edit")).expect("C update 3 (no new token)");
    c.delete(&item(4)).expect("C delete 4 (no new token)");
    c.abort();

    // txn D: an insert then a REKEY of the committed row (rekey carries the source
    // incarnation, allocating nothing), COMMIT.
    let mut d = store.begin();
    record(d.insert(item(6), text("d6")).expect("D insert 6"));
    d.rekey(&item(3), item(30), text("b3-moved")).expect("D rekey 3 -> 30 (no new token)");
    d.commit().expect("D commit");

    tokens
}

#[test]
fn burn_on_allocate_matches_memory_across_aborts() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("incarnburn");
    let instance = InstanceId::new("incarnation-burn-differential");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");

    let m_tokens = burn_script(&mut memory);
    let p_tokens = burn_script(&mut pg);

    // The exact burned sequence: six inserts (2 aborted, 1 committed, 2 aborted,
    // 1 committed) allocate row-0..row-5 in order; update/delete/rekey allocate
    // nothing. Deducible from the contract, independent of either backend.
    let expected: Vec<String> = (0..6).map(|n| format!("row-{n}")).collect();
    assert_eq!(m_tokens, expected, "the reference must burn row-0..row-5 with no reuse");
    assert_eq!(
        p_tokens, m_tokens,
        "burn-on-allocate divergence: the pg incarnation sequence must match the reference \
         op-for-op across aborts — memory={m_tokens:?} pg={p_tokens:?}"
    );

    // Committed survivors carry their allocated incarnation identically. The B-row
    // was rekeyed 3 -> 30 in txn D, preserving its row-2 incarnation on both.
    let m30 = memory.row(&item(30)).expect("mem row 30");
    let p30 = pg.row(&item(30)).expect("pg row 30");
    assert_eq!(m30, p30, "committed rekeyed row divergence at items/30");
    assert_eq!(
        p30.as_ref().map(|r| r.incarnation().as_str().to_owned()),
        Some("row-2".to_owned()),
        "the rekeyed survivor keeps its original row-2 incarnation"
    );
    // The item/6 survivor holds row-5 (the last burned token).
    assert_eq!(
        pg.row(&item(6)).expect("pg row 6").map(|r| r.incarnation().as_str().to_owned()),
        Some("row-5".to_owned()),
        "the last committed insert holds row-5"
    );
}

#[test]
fn burned_tokens_stay_burned_across_reopen() {
    let handle = support::acquire();
    let pg_factory = handle.factory("incarnreopen");
    let instance = InstanceId::new("incarnation-reopen");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    // Alloc tokens 0 and 1 in an ABORTED transition (burned), then commit an insert
    // that allocates token 2. The durable counter is now 3.
    {
        let mut pg = {
            let mut f = pg_factory.clone();
            f.create(instance.clone()).expect("create pg store")
        };
        let mut a = pg.begin();
        assert_eq!(a.insert(item(1), text("a1")).expect("insert 1").as_str(), "row-0");
        assert_eq!(a.insert(item(2), text("a2")).expect("insert 2").as_str(), "row-1");
        a.abort();

        let mut b = pg.begin();
        assert_eq!(b.insert(item(3), text("b3")).expect("insert 3").as_str(), "row-2");
        b.commit().expect("commit b");
    }

    // Reopen the durable schema (no wipe): the next allocation MUST continue at
    // row-3, never recycling any of the three burned tokens (§6.3 durable no-reuse).
    let mut reopened = pg_factory.reopen(instance.clone()).expect("reopen pg store");
    let mut c = reopened.begin();
    let next = c.insert(item(4), text("c4")).expect("post-reopen insert 4");
    assert_eq!(
        next.as_str(),
        "row-3",
        "burned incarnation tokens must survive a reopen: the counter persisted at 3 \
         (two aborted + one committed alloc), so the next token is row-3, not a recycled one"
    );
    c.commit().expect("commit c");
    // `_guard` drops the throwaway schema on scope exit (and on a panic).
}
