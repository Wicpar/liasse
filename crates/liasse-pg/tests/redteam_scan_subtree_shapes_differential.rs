//! RED TEAM — the shape-directed recursive-CTE `scan_subtree` (`crate::read`,
//! §7.6) driven against the in-memory oracle's prefix-range `descends_from` for a
//! memory-vs-pg divergence across the shapes the corpus does not carry as direct
//! `scan_subtree` calls (SPEC-ISSUES item 32: a backend disagreement is always a
//! fix).
//!
//! The oracle enumerates every live row whose address strictly extends `root` and
//! whose every step past the prefix names a collection in `steps`
//! ([`RowAddress::descends_from`]). The pg store must reproduce that EXACT set (and
//! Annex-B order) with a `WITH RECURSIVE` descent that joins children on
//! `step_name = ANY($steps)` and traverses tombstoned intermediates. Attacked here:
//!
//!   * a **self-referential** shape — `children` nested under `children` to depth 4
//!     — plus a shallower sibling (the descent must follow the self-edge, not stop);
//!   * a struct root with **multiple nested collections** (`teams`, `members`) and a
//!     `members` collection appearing at TWO different depths;
//!   * a **tombstoned intermediate** whose live grandchild orphan is still reached
//!     when its step is in `steps`, and NOT reached when only the ancestor step is;
//!   * a root that is ITSELF a tombstone (an auto-created ancestor) with a live
//!     orphan below it (the anchor must resolve the tombstone, unfiltered);
//!   * **auto-created-from-scratch** ancestors between root and a deep leaf;
//!   * the degenerate cases: **empty `steps`** (empty, no query) and a **childless
//!     root** / a step naming a collection with no rows (empty).
//!
//! Resolves the DSN through [`support`] and drops its throwaway schema through a
//! [`support::SchemaGuard`] even on a panic.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress, StoreFactory, StoredRow,
    Transition,
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

/// Insert `address` as a fresh row in its own single-op commit.
fn insert<S: InstanceStore>(store: &mut S, address: RowAddress, payload: &str) {
    let mut txn = store.begin();
    txn.insert(address, text(payload)).expect("insert");
    txn.commit().expect("commit insert");
}

/// Delete `address` (tombstone in pg) in its own commit.
fn delete<S: InstanceStore>(store: &mut S, address: &RowAddress) {
    let mut txn = store.begin();
    txn.delete(address).expect("delete");
    txn.commit().expect("commit delete");
}

/// Build the identical committed shape on a store.
fn build_shape<S: InstanceStore>(store: &mut S) {
    // Self-referential `children` chain to depth 4, plus a shallow sibling.
    insert(store, addr(&[("tree", ikey(1))]), "tree-1");
    insert(store, addr(&[("tree", ikey(1)), ("children", ikey(2))]), "c2");
    insert(store, addr(&[("tree", ikey(1)), ("children", ikey(2)), ("children", ikey(3))]), "c3");
    insert(
        store,
        addr(&[("tree", ikey(1)), ("children", ikey(2)), ("children", ikey(3)), ("children", ikey(4))]),
        "c4",
    );
    insert(store, addr(&[("tree", ikey(1)), ("children", ikey(5))]), "c5");

    // Multi-nested struct: teams + members, members at two depths.
    insert(store, addr(&[("org", ikey(1))]), "org-1");
    insert(store, addr(&[("org", ikey(1)), ("teams", ikey(10))]), "t10");
    insert(store, addr(&[("org", ikey(1)), ("teams", ikey(10)), ("members", ikey(100))]), "m100");
    insert(store, addr(&[("org", ikey(1)), ("teams", ikey(11))]), "t11");
    insert(store, addr(&[("org", ikey(1)), ("members", ikey(200))]), "m200-direct");

    // Tombstoned intermediate: drop teams/10 (its member 100 becomes a live orphan).
    delete(store, &addr(&[("org", ikey(1)), ("teams", ikey(10))]));

    // Auto-created-from-scratch deep leaf: no /ghost/1, no /ghost/1/sub/2 inserted.
    insert(store, addr(&[("ghost", ikey(1)), ("sub", ikey(2)), ("leaf", ikey(3))]), "leaf-3");

    // A childless live root.
    insert(store, addr(&[("empty", ikey(1))]), "empty-1");
}

/// Assert the two backends return an identical `scan_subtree` (membership AND order).
fn assert_subtree_match<A: InstanceStore, B: InstanceStore>(
    memory: &A,
    pg: &B,
    root: &RowAddress,
    steps: &[&str],
    label: &str,
) -> Vec<(RowAddress, StoredRow)> {
    let steps: Vec<String> = steps.iter().map(|s| (*s).to_owned()).collect();
    let m = memory.scan_subtree(root, &steps).expect("memory scan_subtree");
    let p = pg.scan_subtree(root, &steps).expect("pg scan_subtree");
    assert_eq!(
        m, p,
        "{label}: scan_subtree divergence at root `{}` steps {steps:?}\n memory={m:?}\n pg={p:?}",
        root.render()
    );
    m
}

#[test]
fn scan_subtree_shapes_match_memory() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("subtreeshapes");
    let instance = InstanceId::new("scan-subtree-shapes");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");
    build_shape(&mut memory);
    build_shape(&mut pg);

    let tree1 = addr(&[("tree", ikey(1))]);
    let tree1c2 = addr(&[("tree", ikey(1)), ("children", ikey(2))]);
    let org1 = addr(&[("org", ikey(1))]);
    let org1t10 = addr(&[("org", ikey(1)), ("teams", ikey(10))]);
    let ghost1 = addr(&[("ghost", ikey(1))]);
    let ghost1sub2 = addr(&[("ghost", ikey(1)), ("sub", ikey(2))]);
    let empty1 = addr(&[("empty", ikey(1))]);

    // Self-ref descent from the top: reaches every `children` node at any depth.
    let s1 = assert_subtree_match(&memory, &pg, &tree1, &["children"], "self-ref/full");
    assert_eq!(s1.len(), 4, "children 2,3,4,5 are all reachable via the self-edge, got {s1:?}");

    // Self-ref descent from a mid node: only the strictly-deeper chain.
    let s2 = assert_subtree_match(&memory, &pg, &tree1c2, &["children"], "self-ref/mid");
    assert_eq!(s2.len(), 2, "children 3,4 below children/2, got {s2:?}");

    // Multi-collection: both teams and members traversed. teams/10 is a tombstone
    // (not emitted); its orphan member 100 IS reached, plus team 11 and the direct
    // member 200.
    let s3 = assert_subtree_match(&memory, &pg, &org1, &["teams", "members"], "multi/both");
    assert_eq!(s3.len(), 3, "expected m100(orphan), t11, m200; got {s3:?}");

    // Only `teams` in steps: the member step is NOT traversed, so member 100 under
    // the tombstoned team 10 is unreachable and the direct member 200 is excluded;
    // only the live team 11 remains.
    let s4 = assert_subtree_match(&memory, &pg, &org1, &["teams"], "multi/teams-only");
    assert_eq!(s4.len(), 1, "only team 11 (tombstoned team 10 not a row); got {s4:?}");

    // Only `members`: the direct member 200 is reached; member 100 sits under a
    // `teams` edge not in steps, so it is unreachable.
    let s5 = assert_subtree_match(&memory, &pg, &org1, &["members"], "multi/members-only");
    assert_eq!(s5.len(), 1, "only the direct member 200; got {s5:?}");

    // Root is ITSELF a tombstone (deleted team 10) with a live orphan below it.
    let s6 = assert_subtree_match(&memory, &pg, &org1t10, &["members"], "tombstone-root");
    assert_eq!(s6.len(), 1, "the orphan member 100 under the tombstoned team 10; got {s6:?}");

    // Auto-created-from-scratch ancestors between root and a deep leaf.
    let s7 = assert_subtree_match(&memory, &pg, &ghost1, &["sub", "leaf"], "auto-create/full");
    assert_eq!(s7.len(), 1, "the deep leaf 3 through auto-created tombstones; got {s7:?}");
    let s8 = assert_subtree_match(&memory, &pg, &ghost1sub2, &["leaf"], "auto-create/mid");
    assert_eq!(s8.len(), 1, "the deep leaf 3 from the auto-created mid tombstone; got {s8:?}");

    // Degenerate: empty steps → empty (no query); childless root → empty; a step
    // naming a collection with no rows → empty.
    assert!(
        memory.scan_subtree(&org1, &[]).expect("mem empty-steps").is_empty()
            && pg.scan_subtree(&org1, &[]).expect("pg empty-steps").is_empty(),
        "empty steps must yield an empty subtree on both backends"
    );
    let s9 = assert_subtree_match(&memory, &pg, &empty1, &["anything"], "childless-root");
    assert!(s9.is_empty(), "a childless root yields nothing; got {s9:?}");
    let s10 = assert_subtree_match(&memory, &pg, &tree1, &["members"], "no-such-collection");
    assert!(s10.is_empty(), "tree/1 has no `members` children; got {s10:?}");
    // `_guard` drops the throwaway schema on scope exit (and on a panic).
}
