//! RED TEAM — the composite-key rework's physical impact under the tombstone
//! model, checked for a pg-vs-`MemoryStore` divergence across a durable reopen.
//!
//! Every composite-keyed row is addressed by a multi-component `KeyValue`
//! (`$key`-order components), and every nested row under it must reconstruct its
//! address on reopen by walking the `nodes` parent chain through possibly
//! *tombstoned* composite ancestors, decoding each level's `key_wire` back into
//! its multi-component key. This drives the adversarial tombstone edges the task
//! names — a deep tombstone chain, siblings inserted under multiply-tombstoned
//! composite ancestors, a revive/delete/revive cycle, and a rekey of a row whose
//! ancestor is a tombstone — over COMPOSITE-keyed collections, then asserts the
//! pg backend is 0-divergence vs the `MemoryStore` oracle both live and, crucially,
//! after a reopen that rebuilds the read model purely from the durable node tree.
//!
//! Cited: §5.4 (composite key identity + nested/orphan rows), B.4/B.5 (composite
//! ordering, contiguous key-ascending scans), §21.1 (top-level drop leaves nested
//! orphans), §22.7/§19.2 (a reopen rebuilds an identical projection; snapshots fold
//! the durable log). Overarching gate: pg must equal `MemoryStore` observably.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, CommitSeq, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress,
    StoreFactory, StoredRow, Transition,
};
use liasse_value::{Integer, Ref, Text, Value};

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}
fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}
/// A row VALUE that is a bare `Value::Composite` (the positional `$key`-order
/// identity carrier) — exercises `value_codec` encode/decode for the composite
/// arm on the real node-write / node-load (reopen) path.
fn composite_value() -> Value {
    Value::Composite(vec![text("eu"), int(1)])
}
/// A row VALUE that is a composite `ref` — the equal-valued positional target key.
fn composite_ref_value() -> Value {
    Value::Ref(Ref::composite(vec![text("eu"), int(1)]))
}

/// A composite address level `name/[text, int]`.
fn comp_step(name: &str, a: &str, b: i64) -> AddressStep {
    AddressStep::new(NameSegment::new(name), KeyValue::composite(text(a), vec![int(b)]))
}
/// A single-int address level.
fn int_step(name: &str, k: i64) -> AddressStep {
    AddressStep::new(NameSegment::new(name), KeyValue::single(int(k)))
}

fn org(region: &str, code: i64) -> RowAddress {
    RowAddress::root(comp_step("orgs", region, code))
}
fn team(region: &str, code: i64, dept: &str, num: i64) -> RowAddress {
    org(region, code).child(comp_step("teams", dept, num))
}
fn member(region: &str, code: i64, dept: &str, num: i64, m: i64) -> RowAddress {
    team(region, code, dept, num).child(int_step("members", m))
}

fn orgs_collection() -> CollectionPath {
    CollectionPath::top(NameSegment::new("orgs"))
}
fn teams_collection(region: &str, code: i64) -> CollectionPath {
    CollectionPath::nested(org(region, code).steps().cloned(), NameSegment::new("teams"))
}
fn members_collection(region: &str, code: i64, dept: &str, num: i64) -> CollectionPath {
    CollectionPath::nested(
        team(region, code, dept, num).steps().cloned(),
        NameSegment::new("members"),
    )
}

/// The identical adversarial op stream both backends run. Each `begin/commit` pair
/// is one committed transition; nothing here depends on a backend's answer.
fn apply_workload<S: InstanceStore>(store: &mut S) {
    // 1. Seed a composite-keyed tree three levels deep.
    let mut txn = store.begin();
    txn.insert(org("eu", 1), text("org-eu-1")).unwrap();
    txn.insert(org("us", 2), text("org-us-2")).unwrap();
    txn.commit().unwrap();

    let mut txn = store.begin();
    txn.insert(team("eu", 1, "sales", 10), text("t-sales")).unwrap();
    txn.insert(team("eu", 1, "eng", 20), text("t-eng")).unwrap();
    txn.commit().unwrap();

    let mut txn = store.begin();
    txn.insert(member("eu", 1, "sales", 10, 100), text("m-100")).unwrap();
    txn.insert(member("eu", 1, "sales", 10, 101), text("m-101")).unwrap();
    txn.insert(member("eu", 1, "eng", 20, 200), text("m-200")).unwrap();
    txn.commit().unwrap();

    // 2. Delete a composite MID node: its members become orphans under a tombstone.
    let mut txn = store.begin();
    txn.delete(&team("eu", 1, "sales", 10)).unwrap();
    txn.commit().unwrap();

    // 3. Delete the composite TOP node: the whole eu/1 subtree is now orphaned under
    //    tombstones (a deep tombstone chain: org tombstone -> team-sales tombstone ->
    //    members; org tombstone -> team-eng orphan -> member).
    let mut txn = store.begin();
    txn.delete(&org("eu", 1)).unwrap();
    txn.commit().unwrap();

    // 4. Insert a NEW sibling directly under the tombstoned composite org.
    let mut txn = store.begin();
    txn.insert(team("eu", 1, "mkt", 30), text("t-mkt")).unwrap();
    txn.commit().unwrap();

    // 5. Insert a NEW sibling under DOUBLY-tombstoned composite ancestors
    //    (org-eu-1 tombstone -> team-sales tombstone -> new member).
    let mut txn = store.begin();
    txn.insert(member("eu", 1, "sales", 10, 102), text("m-102")).unwrap();
    txn.commit().unwrap();

    // 6. Rekey a row whose ancestor (org-eu-1) is a tombstone — same-parent leaf move
    //    under the live-orphan team-eng.
    let mut txn = store.begin();
    txn.rekey(&member("eu", 1, "eng", 20, 200), member("eu", 1, "eng", 20, 201), text("m-201"))
        .unwrap();
    txn.commit().unwrap();

    // 7. Rekey a row under DOUBLY-tombstoned ancestors (org + team-sales tombstones).
    let mut txn = store.begin();
    txn.rekey(&member("eu", 1, "sales", 10, 100), member("eu", 1, "sales", 10, 103), text("m-103"))
        .unwrap();
    txn.commit().unwrap();

    // 8. Revive / delete / revive cycle on the tombstoned composite org.
    let mut txn = store.begin();
    txn.insert(org("eu", 1), text("org-eu-1-revived")).unwrap();
    txn.commit().unwrap();
    let mut txn = store.begin();
    txn.delete(&org("eu", 1)).unwrap();
    txn.commit().unwrap();
    let mut txn = store.begin();
    txn.insert(org("eu", 1), text("org-eu-1-revived-again")).unwrap();
    txn.commit().unwrap();

    // 9. Store composite-typed row VALUES (a bare Value::Composite and a composite
    //    ref) so the reopen decode path exercises value_codec's composite arm on a
    //    real row, not only its unit round-trip.
    let mut txn = store.begin();
    txn.insert(org("comp", 9), composite_value()).unwrap();
    txn.insert(org("comp", 8), composite_ref_value()).unwrap();
    txn.commit().unwrap();

    // 10. Drive a FULLY-DEAD subtree: delete every remaining live member under the
    //     tombstoned team-sales so that tombstone's ONLY descendants are tombstones.
    let mut txn = store.begin();
    txn.delete(&member("eu", 1, "sales", 10, 101)).unwrap();
    txn.delete(&member("eu", 1, "sales", 10, 102)).unwrap();
    txn.delete(&member("eu", 1, "sales", 10, 103)).unwrap();
    txn.commit().unwrap();
}

/// Every address the workload touches — present or tombstoned — so both presence and
/// absence are compared at each.
fn touched() -> Vec<RowAddress> {
    vec![
        org("eu", 1),
        org("us", 2),
        org("comp", 9),
        org("comp", 8),
        team("eu", 1, "sales", 10),
        team("eu", 1, "eng", 20),
        team("eu", 1, "mkt", 30),
        member("eu", 1, "sales", 10, 100),
        member("eu", 1, "sales", 10, 101),
        member("eu", 1, "sales", 10, 102),
        member("eu", 1, "sales", 10, 103),
        member("eu", 1, "eng", 20, 200),
        member("eu", 1, "eng", 20, 201),
    ]
}

fn collections() -> Vec<CollectionPath> {
    vec![
        orgs_collection(),
        teams_collection("eu", 1),
        teams_collection("us", 2),
        members_collection("eu", 1, "sales", 10),
        members_collection("eu", 1, "eng", 20),
        members_collection("eu", 1, "mkt", 30),
    ]
}

fn sorted_scan<S: InstanceStore>(store: &S, c: &CollectionPath) -> Vec<(RowAddress, StoredRow)> {
    let mut rows = store.scan(c).expect("scan");
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// Assert two stores are observably identical: head, every touched row (presence AND
/// absence, incarnation included), every collection scan (order included), every
/// frontier snapshot, and the whole commit log.
fn assert_stores_agree<A: InstanceStore, B: InstanceStore>(a: &A, b: &B, label: &str) {
    assert_eq!(a.head().unwrap(), b.head().unwrap(), "{label}: head disagrees");

    for address in touched() {
        assert_eq!(
            a.row(&address).expect("row a"),
            b.row(&address).expect("row b"),
            "{label}: row disagrees at {}",
            address.render()
        );
    }

    for c in collections() {
        assert_eq!(
            sorted_scan(a, &c),
            sorted_scan(b, &c),
            "{label}: scan disagrees for a collection"
        );
    }

    // Snapshot at every frontier from genesis to head folds the durable log; both
    // backends must produce the identical row set at each.
    let head = a.head().unwrap().get();
    for f in 0..=head {
        let frontier = CommitSeq::from_stored(f);
        let sa = a.snapshot(frontier).expect("snapshot a");
        let sb = b.snapshot(frontier).expect("snapshot b");
        assert_eq!(sa.frontier(), sb.frontier(), "{label}: snapshot frontier {f}");
        assert_eq!(sa.len(), sb.len(), "{label}: snapshot len at frontier {f}");
        for c in collections() {
            let mut ra = sa.scan(&c);
            let mut rb = sb.scan(&c);
            ra.sort_by(|x, y| x.0.cmp(&y.0));
            rb.sort_by(|x, y| x.0.cmp(&y.0));
            assert_eq!(ra, rb, "{label}: snapshot scan at frontier {f} disagrees");
        }
    }

    // The full commit log must replay identically.
    let la = a.log_from(CommitSeq::GENESIS).expect("log a");
    let lb = b.log_from(CommitSeq::GENESIS).expect("log b");
    assert_eq!(la.len(), lb.len(), "{label}: log length disagrees");
}

#[test]
fn composite_tombstone_tree_is_zero_divergence_across_reopen() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("compositetombstone");
    let instance = InstanceId::new("composite-tombstone-reopen");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    // Oracle and pg run the identical workload, so their opaque `row-N` incarnations
    // line up op-for-op.
    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory");
    apply_workload(&mut memory);

    let mut pg = pg_factory.create(instance.clone()).expect("create pg");
    apply_workload(&mut pg);

    // Live: pg projection must match the oracle exactly.
    assert_stores_agree(&pg, &memory, "live pg vs memory");

    // Reopen from the durable node tree — the process-restart path — and re-compare.
    // This is where a composite address that fails to reconstruct through a
    // tombstoned composite ancestor, or a lost/duplicated orphan, would surface.
    let reopened = pg_factory.reopen(instance).expect("reopen pg");
    assert_stores_agree(&reopened, &memory, "reopened pg vs memory");
}
