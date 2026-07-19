//! RED TEAM — a rich, orphan-and-rekey-heavy committed history driven through
//! BOTH store backends and cross-checked for a memory-vs-pg divergence at EVERY
//! serial frontier, plus a durable reopen (SPEC-ISSUES item 32: a backend
//! disagreement is always a fix).
//!
//! The corpus gate `node_tree_consistency::head_fast_path_equals_log_fold` proves
//! the Phase-6 `snapshot(head)` fast path equals the log fold for ONE int-keyed
//! workload with a leaf rekey and a single non-leaf delete. This attacks the edges
//! that gate does not reach, all in one identical op stream on both backends:
//!
//!   * a **non-leaf ancestor rekey** whose moved row has a DEEP (depth-3/4) subtree
//!     — the reference store leaves those descendants where they are (§5.4 "rekey
//!     moves ONLY the addressed row"); the pg tree tombstones the source and keeps
//!     the descendants as orphans, and the head fast path must reconstruct their
//!     full addresses THROUGH the tombstoned ancestor;
//!   * a **non-leaf mid-node rekey** (`…/c/100 -> …/c/200`) that leaves a live
//!     grandchild orphaned at the OLD mid address;
//!   * a **non-leaf delete** that orphans a whole depth-4 subtree;
//!   * a **revive over a tombstone** (`insert /a/1` after `/a/1` was rekeyed away)
//!     whose orphan descendants must re-associate with the revived live row;
//!   * an **auto-create-from-scratch** nested insert (`/z/5/y/50/x/500` with no
//!     ancestor ever inserted);
//!   * **composite keys** and **edge-byte keys** (`text` with an interior `U+0000`,
//!     the empty `text`, and `i64::MAX`) so the `key_enc`/`key_wire` columns are
//!     exercised on the address-reconstruction path, not just at genesis.
//!
//! The cross-checks (all backend-agnostic, deducible from the `liasse-store`
//! contract, not from either backend answering):
//!
//!   1. every commit's `Result<CommitOutcome, StoreError>` and the resulting head
//!      match op-for-op (admission parity);
//!   2. `snapshot(f)` is byte-identical between the two backends for EVERY frontier
//!      `f in GENESIS..=head` — this spans the `frontier == head` fast path, the
//!      `frontier < head` log fold, AND the head/head-1 boundary in one sweep;
//!   3. `row`/`scan` at head agree at every touched (present AND absent) address;
//!   4. `log_from(GENESIS)` is identical;
//!   5. after a durable reopen of the pg store, (2)+(3) still hold — the tree the
//!      reopen reads back reproduces the reference's observable state (§22.7/§19.2).
//!
//! Resolves the DSN through [`support`] and drops its throwaway schema through a
//! [`support::SchemaGuard`] even on a panic.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines
)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, CommitOutcome, CommitSeq, InstanceStore, KeyValue,
    MemoryStoreFactory, RowAddress, StoreError, StoreFactory, StoredRow, Transition,
};
use liasse_value::{Integer, Text, Value};

// ----- key / address builders -------------------------------------------------

fn ikey(k: i64) -> KeyValue {
    KeyValue::single(Value::Int(Integer::from(k)))
}
fn tkey(k: &str) -> KeyValue {
    KeyValue::single(Value::Text(Text::new(k)))
}
/// A composite `(text, int)` key — the `/orgs/{eu,1}` shape.
fn ckey(t: &str, n: i64) -> KeyValue {
    KeyValue::composite(Value::Text(Text::new(t)), [Value::Int(Integer::from(n))])
}
fn text(payload: &str) -> Value {
    Value::Text(Text::new(payload))
}

/// Build a row address from `(name, key)` levels, root first.
fn addr(levels: &[(&str, KeyValue)]) -> RowAddress {
    let mut it = levels.iter();
    let (n0, k0) = it.next().expect("address has at least one level");
    let mut a = RowAddress::root(AddressStep::new(NameSegment::new(*n0), k0.clone()));
    for (n, k) in it {
        a = a.child(AddressStep::new(NameSegment::new(*n), k.clone()));
    }
    a
}

/// The collection an address belongs to (its address minus the final key).
fn coll(address: &RowAddress) -> CollectionPath {
    address.collection()
}

// ----- differential driver ----------------------------------------------------

/// One staged-then-committed operation.
#[derive(Clone)]
enum Op {
    Insert(RowAddress, Value),
    Update(RowAddress, Value),
    Delete(RowAddress),
    Rekey(RowAddress, RowAddress, Value),
}

/// Apply one batch (= one transition = one commit) and return the outcome so the
/// two backends' admission verdicts can be compared directly.
fn apply_batch<S: InstanceStore>(store: &mut S, batch: &[Op]) -> Result<CommitOutcome, StoreError> {
    let mut txn = store.begin();
    for op in batch {
        match op {
            Op::Insert(a, v) => {
                txn.insert(a.clone(), v.clone())?;
            }
            Op::Update(a, v) => {
                txn.update(a, v.clone())?;
            }
            Op::Delete(a) => {
                txn.delete(a)?;
            }
            Op::Rekey(f, t, v) => {
                txn.rekey(f, t.clone(), v.clone())?;
            }
        }
    }
    txn.commit()
}

/// The adversarial history, as a list of commits. Every op is admitted by BOTH
/// backends (staging is occupancy-only in both), so the two run the identical
/// stream and their opaque `row-N` incarnations line up op-for-op.
fn history() -> Vec<Vec<Op>> {
    let a = |k: i64| addr(&[("a", ikey(k))]);
    let ab = |k: i64, k2: i64| addr(&[("a", ikey(k)), ("b", ikey(k2))]);
    let abc = |k: i64, k2: i64, k3: i64| addr(&[("a", ikey(k)), ("b", ikey(k2)), ("c", ikey(k3))]);
    let abcd = |k: i64, k2: i64, k3: i64, k4: i64| {
        addr(&[("a", ikey(k)), ("b", ikey(k2)), ("c", ikey(k3)), ("d", ikey(k4))])
    };
    vec![
        // 1: top level
        vec![
            Op::Insert(a(1), text("a1")),
            Op::Insert(a(2), text("a2")),
            Op::Insert(a(3), text("a3")),
        ],
        // 2: depth 2
        vec![
            Op::Insert(ab(1, 10), text("b1-10")),
            Op::Insert(ab(1, 11), text("b1-11")),
            Op::Insert(ab(2, 20), text("b2-20")),
        ],
        // 3: depth 3
        vec![
            Op::Insert(abc(1, 10, 100), text("c-100")),
            Op::Insert(abc(1, 10, 101), text("c-101")),
            Op::Insert(abc(1, 11, 110), text("c-110")),
        ],
        // 4: depth 4
        vec![Op::Insert(abcd(1, 10, 100, 1000), text("d-1000"))],
        // 5: updates at two depths
        vec![
            Op::Update(a(1), text("a1-v2")),
            Op::Update(abcd(1, 10, 100, 1000), text("d-1000-v2")),
        ],
        // 6: leaf tombstone
        vec![Op::Delete(abc(1, 11, 110))],
        // 7: NON-LEAF mid-node rekey — c/100 (has child d/1000) -> c/200; the deep
        //    grandchild orphans at the OLD /a/1/b/10/c/100/d/1000 address.
        vec![Op::Rekey(abc(1, 10, 100), abc(1, 10, 200), text("c-200"))],
        // 8: NON-LEAF delete — dropping /a/1/b/10 orphans c/101, c/200, and the
        //    deep d/1000 all under a tombstoned mid-ancestor.
        vec![Op::Delete(ab(1, 10))],
        // 9: NON-LEAF ancestor rekey — /a/1 -> /a/9; the whole depth-4 subtree
        //    orphans under the tombstoned /a/1.
        vec![Op::Rekey(a(1), a(9), text("a9"))],
        // 10: auto-create-from-scratch nested insert, depth 3.
        vec![Op::Insert(addr(&[("z", ikey(5)), ("y", ikey(50)), ("x", ikey(500))]), text("x-500"))],
        // 11: revive /a/1 over the tombstone left by the step-9 rekey; its orphan
        //     descendants must re-associate with the revived live row.
        vec![Op::Insert(a(1), text("a1-revived"))],
        // 12: composite-keyed rows.
        vec![
            Op::Insert(addr(&[("orgs", ckey("eu", 1))]), text("org-eu-1")),
            Op::Insert(addr(&[("orgs", ckey("eu", 1)), ("teams", ikey(7))]), text("team-7")),
        ],
        // 13: edge-byte keys — interior NUL, empty text, and i64::MAX.
        vec![
            Op::Insert(addr(&[("edge", tkey("a\u{0}b"))]), text("nul-key")),
            Op::Insert(addr(&[("edge", tkey(""))]), text("empty-key")),
            Op::Insert(addr(&[("big", ikey(i64::MAX))]), text("max-int")),
        ],
    ]
}

/// Every address the workload touches, so `row` parity is checked at present rows,
/// tombstones, and never-existed positions alike.
fn probe_addresses() -> Vec<RowAddress> {
    vec![
        addr(&[("a", ikey(1))]),
        addr(&[("a", ikey(2))]),
        addr(&[("a", ikey(3))]),
        addr(&[("a", ikey(9))]),
        addr(&[("a", ikey(1)), ("b", ikey(10))]),
        addr(&[("a", ikey(1)), ("b", ikey(11))]),
        addr(&[("a", ikey(2)), ("b", ikey(20))]),
        addr(&[("a", ikey(1)), ("b", ikey(10)), ("c", ikey(100))]),
        addr(&[("a", ikey(1)), ("b", ikey(10)), ("c", ikey(101))]),
        addr(&[("a", ikey(1)), ("b", ikey(10)), ("c", ikey(200))]),
        addr(&[("a", ikey(1)), ("b", ikey(11)), ("c", ikey(110))]),
        addr(&[("a", ikey(1)), ("b", ikey(10)), ("c", ikey(100)), ("d", ikey(1000))]),
        addr(&[("z", ikey(5))]),
        addr(&[("z", ikey(5)), ("y", ikey(50))]),
        addr(&[("z", ikey(5)), ("y", ikey(50)), ("x", ikey(500))]),
        addr(&[("orgs", ckey("eu", 1))]),
        addr(&[("orgs", ckey("eu", 1)), ("teams", ikey(7))]),
        addr(&[("edge", tkey("a\u{0}b"))]),
        addr(&[("edge", tkey(""))]),
        addr(&[("big", ikey(i64::MAX))]),
    ]
}

/// Every collection the workload populates, for `scan` parity (Annex B order).
fn probe_collections() -> Vec<CollectionPath> {
    probe_addresses().iter().map(coll).collect()
}

fn assert_reads_match<A: InstanceStore, B: InstanceStore>(memory: &A, pg: &B, label: &str) {
    for address in probe_addresses() {
        let m = memory.row(&address).expect("memory row");
        let p = pg.row(&address).expect("pg row");
        assert_eq!(
            m,
            p,
            "{label}: row divergence at {} — memory={m:?} pg={p:?}",
            address.render()
        );
    }
    for collection in probe_collections() {
        let m: Vec<(RowAddress, StoredRow)> = memory.scan(&collection).expect("memory scan");
        let p: Vec<(RowAddress, StoredRow)> = pg.scan(&collection).expect("pg scan");
        assert_eq!(
            m, p,
            "{label}: scan divergence for collection `{}` (order + membership must match)",
            collection.name().as_str()
        );
    }
}

#[test]
fn snapshot_at_every_frontier_matches_memory() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("snapfrontier");
    let instance = InstanceId::new("snapshot-frontier-differential");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");

    // (1) Admission parity — compare each commit's verdict and the resulting head.
    for (i, batch) in history().iter().enumerate() {
        let m = apply_batch(&mut memory, batch);
        let p = apply_batch(&mut pg, batch);
        assert_eq!(m, p, "commit {i}: admission verdict diverged — memory={m:?} pg={p:?}");
        let mh = memory.head().expect("memory head");
        let ph = pg.head().expect("pg head");
        assert_eq!(mh, ph, "commit {i}: head diverged — memory={mh:?} pg={ph:?}");
    }

    let head = memory.head().expect("memory head");
    assert!(head.get() >= 13, "the workload must produce a deep history, got head {}", head.get());

    // (2) snapshot parity at EVERY frontier: fast path (== head), log fold (< head),
    //     and the head/head-1 boundary — all in one sweep.
    for f in 0..=head.get() {
        let frontier = CommitSeq::from_stored(f);
        let m = memory.snapshot(frontier).expect("memory snapshot");
        let p = pg.snapshot(frontier).expect("pg snapshot");
        assert_eq!(
            m, p,
            "snapshot divergence at frontier {f} (head={}): the pg {} must be byte-identical \
             to the in-memory log fold",
            head.get(),
            if f == head.get() { "head fast path" } else { "commit-log fold" }
        );
    }

    // (3) row/scan parity at head.
    assert_reads_match(&memory, &pg, "live");

    // (4) log parity.
    let m_log = memory.log_from(CommitSeq::GENESIS).expect("memory log");
    let p_log = pg.log_from(CommitSeq::GENESIS).expect("pg log");
    assert_eq!(m_log, p_log, "commit-log divergence between the two backends");

    // (5) durable reopen: the pg tree read back must reproduce the reference.
    let reopened = pg_factory.reopen(instance.clone()).expect("reopen pg store");
    assert_eq!(
        reopened.head().expect("reopened head"),
        head,
        "reopen must recover the durable head"
    );
    let m_head_snap = memory.snapshot(head).expect("memory head snapshot");
    let r_head_snap = reopened.snapshot(head).expect("reopened head snapshot");
    assert_eq!(
        m_head_snap, r_head_snap,
        "reopen (§22.7/§19.2): the reopened head fast path must reproduce the reference live set"
    );
    assert_reads_match(&memory, &reopened, "reopened");
    // `_guard` drops the throwaway schema on scope exit (and on a panic).
}
