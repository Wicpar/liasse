//! RED TEAM — the [`PgTransition`] staging overlay (committed SQL base + in-memory
//! overlay) driven against the in-memory reference's overlay for a memory-vs-pg
//! divergence, plus read-your-committed-writes through the r2d2 read pool
//! (SPEC-ISSUES item 32: a backend disagreement is always a fix).
//!
//! `PgTransition::row`/`scan` read the *committed* state live from SQL
//! (`crate::read`, the pooled §4.1/§4.2 statements) and then shadow it with the
//! staged overlay — a staged put appears/overrides, a staged delete hides a base
//! row, a staged rekey hides `from` and reveals `to`. The reference
//! `MemoryTransition` overlays the same way over its in-process `current` map. The
//! contract binds them to identical staged reads (read-your-writes, §22.2), so the
//! two must agree op-for-op on:
//!
//!   * a staged INSERT of a fresh row (appears in `row`/`scan`);
//!   * a staged UPDATE overriding a committed base value;
//!   * a staged DELETE hiding a committed base row;
//!   * a staged REKEY hiding `from` and revealing `to` in the same scan;
//!   * an INSERT-then-DELETE in the SAME staging (net absent);
//!   * a staged INSERT into a NESTED collection (visible in that nested scan);
//!   * the untouched committed base still visible under the overlay.
//!
//! Then, after a real commit, [`InstanceStore::row`]/`scan` served from a POOLED
//! read connection must observe the freshly committed writes (read-your-committed-
//! writes across the writer→pool boundary, §5.4), matching the reference.
//!
//! Resolves the DSN through [`support`] and drops its throwaway schema through a
//! [`support::SchemaGuard`] even on a panic.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::type_complexity)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress,
    StoreFactory, StoredRow, Transition,
};
use liasse_value::{Integer, Text, Value};

fn ikey(k: i64) -> KeyValue {
    KeyValue::single(Value::Int(Integer::from(k)))
}
fn text(payload: &str) -> Value {
    Value::Text(Text::new(payload))
}
fn addr(levels: &[(&str, KeyValue)]) -> RowAddress {
    let mut it = levels.iter();
    let (n0, k0) = it.next().expect("address has at least one level");
    let mut a = RowAddress::root(AddressStep::new(NameSegment::new(*n0), k0.clone()));
    for (n, k) in it {
        a = a.child(AddressStep::new(NameSegment::new(*n), k.clone()));
    }
    a
}

fn item(k: i64) -> RowAddress {
    addr(&[("items", ikey(k))])
}
fn sub(k: i64, s: i64) -> RowAddress {
    addr(&[("items", ikey(k)), ("sub", ikey(s))])
}
fn deep(k: i64, d: i64) -> RowAddress {
    addr(&[("items", ikey(k)), ("deep", ikey(d))])
}

/// Seed the identical committed base into a store.
fn seed_base<S: InstanceStore>(store: &mut S) {
    let mut txn = store.begin();
    txn.insert(item(1), text("one")).expect("seed items/1");
    txn.insert(item(2), text("two")).expect("seed items/2");
    txn.insert(item(3), text("three")).expect("seed items/3");
    txn.insert(sub(1, 10), text("sub-10")).expect("seed items/1/sub/10");
    txn.insert(sub(1, 11), text("sub-11")).expect("seed items/1/sub/11");
    txn.commit().expect("commit base");
}

/// Stage the mixed overlay, then read every probe THROUGH the transition and
/// return the results, aborting the transition (committed state untouched).
fn stage_and_probe<S: InstanceStore>(
    store: &mut S,
    probes: &[RowAddress],
    collections: &[CollectionPath],
) -> (Vec<Option<StoredRow>>, Vec<Vec<(RowAddress, StoredRow)>>) {
    let mut txn = store.begin();
    txn.insert(item(4), text("four-staged")).expect("stage insert items/4");
    txn.update(&item(2), text("two-staged")).expect("stage update items/2");
    txn.delete(&item(3)).expect("stage delete items/3");
    txn.rekey(&item(1), item(9), text("nine-staged")).expect("stage rekey items/1 -> items/9");
    txn.insert(deep(5, 50), text("deep-staged")).expect("stage nested insert items/5/deep/50");
    txn.insert(item(7), text("seven-transient")).expect("stage insert items/7");
    txn.delete(&item(7)).expect("stage delete items/7 (net absent)");

    let rows = probes.iter().map(|a| txn.row(a).expect("txn row")).collect();
    let scans = collections.iter().map(|c| txn.scan(c).expect("txn scan")).collect();
    txn.abort();
    (rows, scans)
}

#[test]
fn staged_overlay_reads_match_memory() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("stagingoverlay");
    let instance = InstanceId::new("staging-overlay-differential");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");
    seed_base(&mut memory);
    seed_base(&mut pg);

    let probes = vec![
        item(1), // rekey from -> hidden
        item(9), // rekey to   -> revealed
        item(2), // updated    -> staged value
        item(3), // deleted    -> hidden
        item(4), // staged insert
        item(7), // inserted then deleted -> absent
        deep(5, 50), // nested staged insert
        sub(1, 10),  // untouched base
    ];
    let collections = vec![
        CollectionPath::top(NameSegment::new("items")),
        CollectionPath::nested(item(1).steps().cloned(), NameSegment::new("sub")),
        CollectionPath::nested(item(5).steps().cloned(), NameSegment::new("deep")),
    ];

    let (m_rows, m_scans) = stage_and_probe(&mut memory, &probes, &collections);
    let (p_rows, p_scans) = stage_and_probe(&mut pg, &probes, &collections);

    for (i, address) in probes.iter().enumerate() {
        assert_eq!(
            m_rows[i], p_rows[i],
            "staged `row` divergence at {} — memory={:?} pg={:?}",
            address.render(),
            m_rows[i],
            p_rows[i]
        );
    }
    for (i, collection) in collections.iter().enumerate() {
        assert_eq!(
            m_scans[i], p_scans[i],
            "staged `scan` divergence for `{}` (order + overlay must match) — memory={:?} pg={:?}",
            collection.name().as_str(),
            m_scans[i],
            p_scans[i]
        );
    }

    // Sanity on the reference itself, so a silent double-empty cannot pass: the
    // staged top-level scan holds exactly items {2(staged), 4, 9} in key order.
    let top = &m_scans[0];
    let keys: Vec<&Value> = top
        .iter()
        .map(|(a, _)| a.steps().last().expect("has step").key().components().next().expect("k"))
        .collect();
    assert_eq!(
        keys,
        vec![
            &Value::Int(Integer::from(2)),
            &Value::Int(Integer::from(4)),
            &Value::Int(Integer::from(9)),
        ],
        "the staged top-level scan must be exactly items 2,4,9 in Annex B order, got {top:?}"
    );

    // The aborted staging left committed state untouched: base items {1,2,3} live.
    for (k, v) in [(1, "one"), (2, "two"), (3, "three")] {
        let m = memory.row(&item(k)).expect("memory row");
        let p = pg.row(&item(k)).expect("pg row");
        assert_eq!(m, p, "post-abort committed row divergence at items/{k}");
        assert_eq!(
            p.map(|r| r.value().clone()),
            Some(text(v)),
            "abort must not perturb committed items/{k}"
        );
    }
}

#[test]
fn read_your_committed_writes_through_pool() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("ryow");
    let instance = InstanceId::new("read-your-committed-writes");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");

    // A sequence of commits, each immediately followed by a pooled read that must
    // observe the just-committed write (writer transaction committed → the separate
    // pooled read connection sees it under READ COMMITTED, §5.4).
    for round in 0..5 {
        let a = item(round);
        let mut mtxn = memory.begin();
        mtxn.insert(a.clone(), text(&format!("v-{round}"))).expect("mem insert");
        mtxn.commit().expect("mem commit");
        let mut ptxn = pg.begin();
        ptxn.insert(a.clone(), text(&format!("v-{round}"))).expect("pg insert");
        ptxn.commit().expect("pg commit");

        // Read-your-committed-writes: the freshly committed row is visible NOW.
        let mr = memory.row(&a).expect("mem row");
        let pr = pg.row(&a).expect("pg row (pooled)");
        assert_eq!(mr, pr, "round {round}: pooled read did not observe the just-committed write");
        assert!(pr.is_some(), "round {round}: committed row must be visible immediately");

        // And the whole collection scan (a multi-statement pooled read path) is
        // coherent with the reference after each commit.
        let mc = memory.scan(&CollectionPath::top(NameSegment::new("items"))).expect("mem scan");
        let pc = pg.scan(&CollectionPath::top(NameSegment::new("items"))).expect("pg scan");
        assert_eq!(mc, pc, "round {round}: pooled scan diverged from the reference");
        assert_eq!(pc.len(), round as usize + 1, "round {round}: scan must see every commit so far");
    }
}
