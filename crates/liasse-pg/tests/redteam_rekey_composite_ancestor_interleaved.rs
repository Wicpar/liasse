//! RED TEAM — the task's headline pg-vs-`MemoryStore` scenario, the combination
//! the existing corpus does not run as one transition stream:
//!
//!   * a **rekey of a composite-keyed ANCESTOR** row (`/orgs/{eu,1}`) that itself
//!     has a **live** descendant (`teams/{sales,10}`) AND a **tombstoned**
//!     descendant (`teams/{eng,20}`) hanging off it — §5.4 moves only the addressed
//!     row, leaving the descendants as orphans under the source tombstone (the
//!     existing composite test only rekeys leaf *members*, never the ancestor);
//!   * `set_definition` / `set_composition` **interleaved** with node ops in the
//!     same commit stream (D.4 definition, §19.5 composition);
//!   * blobs (§18) and history points (§19.3) recorded between commits;
//!   * a revive of the rekeyed-away ancestor address, and a second rekey of the
//!     surviving orphan.
//!
//! Every op is a valid store-contract call (a nested insert always follows an
//! ancestor that was, at some point, a node — either live or a tombstone). The one
//! gate: the PostgreSQL backend must be observably identical to the in-memory
//! reference — head, every touched row (presence AND absence, incarnation
//! included), every collection scan (Annex B order), every frontier snapshot
//! (§22.7/§19.2), the whole commit log, the active definition and composition,
//! every history point, and every blob — both **live** and, crucially, after a
//! **reopen** that rebuilds the read model purely from the durable node tree.
//!
//! Cited: §5.4 (composite identity + nested/orphan rows, atomic rekey), B.4/B.5
//! (composite ordering, key-ascending scans), §21.1 (drops leave nested orphans),
//! §22.7/§19.2 (reopen rebuilds an identical projection; snapshots fold the log),
//! D.4/§19.5 (definition/composition), §18/§19.3 (blobs, points).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_ident::{HistoryPoint, InstanceId, LineageId, NameSegment, PointId};
use liasse_store::{
    AddressStep, CollectionPath, CommitSeq, Composition, DefinitionText, InstanceStore, KeyValue,
    MemoryStoreFactory, Mount, RowAddress, StoreFactory, StoredRow, Transition,
};
use liasse_value::{Integer, Sha512, Text, Value};

use support::SchemaGuard;

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}
fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}

/// A composite address level `name/[region, code]`.
fn comp_step(name: &str, region: &str, code: i64) -> AddressStep {
    AddressStep::new(NameSegment::new(name), KeyValue::composite(text(region), vec![int(code)]))
}

fn org(region: &str, code: i64) -> RowAddress {
    RowAddress::root(comp_step("orgs", region, code))
}
fn team(region: &str, code: i64, dept: &str, num: i64) -> RowAddress {
    org(region, code).child(comp_step("teams", dept, num))
}

fn orgs_collection() -> CollectionPath {
    CollectionPath::top(NameSegment::new("orgs"))
}
fn teams_collection(region: &str, code: i64) -> CollectionPath {
    CollectionPath::nested(org(region, code).steps().cloned(), NameSegment::new("teams"))
}

fn point(lineage: &str, id: &str) -> HistoryPoint {
    HistoryPoint::new(LineageId::new(lineage), PointId::new(id))
}

fn composition(name: &str, instance: &str, lineage: &str, pt: &str) -> Composition {
    Composition::new().with(name, Mount::new(InstanceId::new(instance), point(lineage, pt)))
}

/// The identical adversarial op stream both backends run. Each `begin/commit` pair
/// is one committed transition. Blob puts and point records happen between commits.
/// Returns the two blob digests recorded, in order.
fn apply_workload<S: InstanceStore>(store: &mut S) -> (Sha512, Sha512) {
    // 1. Seed two composite-keyed orgs; carry a definition in the SAME commit.
    let mut txn = store.begin();
    txn.insert(org("eu", 1), text("org-eu-1")).unwrap();
    txn.insert(org("us", 2), text("org-us-2")).unwrap();
    txn.set_definition(DefinitionText::new("{ \"$liasse\": 1, \"v\": 1 }"));
    txn.commit().unwrap();

    // 2. Two teams under eu/1: one will stay live, one will be tombstoned.
    let mut txn = store.begin();
    txn.insert(team("eu", 1, "sales", 10), text("t-sales")).unwrap();
    txn.insert(team("eu", 1, "eng", 20), text("t-eng")).unwrap();
    txn.commit().unwrap();

    // 3. Tombstone the eng team; carry a composition in the SAME commit.
    let mut txn = store.begin();
    txn.delete(&team("eu", 1, "eng", 20)).unwrap();
    txn.set_composition(composition("child", "inst-a", "lin-a", "p1"));
    txn.commit().unwrap();

    // A blob and a history point recorded at this head, between commits.
    let blob_a = store.put_blob(b"blob-A-bytes").unwrap();
    store.record_point(store.head().unwrap(), point("lin-main", "before-rekey")).unwrap();

    // 4. THE HEADLINE: rekey the composite ANCESTOR org(eu,1) -> org(gb,3), while it
    //    has a LIVE descendant (teams/sales/10) and a TOMBSTONED descendant
    //    (teams/eng/20). §5.4 moves only the org row; both descendants become orphans
    //    under the eu/1 source tombstone.
    let mut txn = store.begin();
    txn.rekey(&org("eu", 1), org("gb", 3), text("org-eu-1-moved")).unwrap();
    txn.commit().unwrap();

    // 5. Insert a NEW team under the now-tombstoned eu/1 ancestor (reachable: its node
    //    still exists as a tombstone). New definition in the same commit.
    let mut txn = store.begin();
    txn.insert(team("eu", 1, "mkt", 30), text("t-mkt")).unwrap();
    txn.set_definition(DefinitionText::new("{ \"$liasse\": 1, \"v\": 2 }"));
    txn.commit().unwrap();

    // 6. Revive the rekeyed-away ancestor address eu/1 (its tombstone comes back live,
    //    re-parenting the orphans that hung off it).
    let mut txn = store.begin();
    txn.insert(org("eu", 1), text("org-eu-1-revived")).unwrap();
    txn.commit().unwrap();

    let blob_b = store.put_blob(b"blob-B-bytes").unwrap();
    store.record_point(store.head().unwrap(), point("lin-main", "after-revive")).unwrap();

    // 7. Rekey the surviving live orphan leaf under the revived ancestor; new
    //    composition in the same commit.
    let mut txn = store.begin();
    txn.rekey(&team("eu", 1, "sales", 10), team("eu", 1, "sales", 11), text("t-sales-moved"))
        .unwrap();
    txn.set_composition(composition("child", "inst-a", "lin-a", "p2"));
    txn.commit().unwrap();

    (blob_a, blob_b)
}

/// Every address the workload touches — present or tombstoned — so both presence and
/// absence are compared at each.
fn touched() -> Vec<RowAddress> {
    vec![
        org("eu", 1),
        org("us", 2),
        org("gb", 3),
        team("eu", 1, "sales", 10),
        team("eu", 1, "sales", 11),
        team("eu", 1, "eng", 20),
        team("eu", 1, "mkt", 30),
        team("gb", 3, "sales", 10), // must be ABSENT on both (rekey does not move the subtree)
    ]
}

fn collections() -> Vec<CollectionPath> {
    vec![
        orgs_collection(),
        teams_collection("eu", 1),
        teams_collection("gb", 3),
        teams_collection("us", 2),
    ]
}

fn sorted_scan<S: InstanceStore>(store: &S, c: &CollectionPath) -> Vec<(RowAddress, StoredRow)> {
    let mut rows = store.scan(c).expect("scan");
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// Assert two stores are observably identical across the whole contract surface.
fn assert_stores_agree<A: InstanceStore, B: InstanceStore>(
    a: &A,
    b: &B,
    blobs: (Sha512, Sha512),
    label: &str,
) {
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
        assert_eq!(sorted_scan(a, &c), sorted_scan(b, &c), "{label}: scan disagrees");
    }

    // Snapshot at every frontier from genesis to head folds the durable log.
    let head = a.head().unwrap().get();
    for f in 0..=head {
        let frontier = CommitSeq::from_stored(f);
        assert_eq!(
            a.snapshot(frontier).expect("snapshot a"),
            b.snapshot(frontier).expect("snapshot b"),
            "{label}: snapshot at frontier {f} disagrees"
        );
    }

    // The whole commit log (ops, incarnations, transaction ids) must match.
    assert_eq!(
        a.log_from(CommitSeq::GENESIS).expect("log a"),
        b.log_from(CommitSeq::GENESIS).expect("log b"),
        "{label}: commit log disagrees"
    );

    // Definition and composition — the metadata carried inline with node ops.
    assert_eq!(a.definition().unwrap(), b.definition().unwrap(), "{label}: definition disagrees");
    assert_eq!(a.composition().unwrap(), b.composition().unwrap(), "{label}: composition disagrees");

    // Every history point maps to the same position.
    for p in [point("lin-main", "before-rekey"), point("lin-main", "after-revive")] {
        assert_eq!(
            a.point_position(&p).unwrap(),
            b.point_position(&p).unwrap(),
            "{label}: history point position disagrees"
        );
    }

    // Blobs round-trip identically.
    let (blob_a, blob_b) = blobs;
    for digest in [blob_a, blob_b] {
        assert_eq!(a.has_blob(&digest).unwrap(), b.has_blob(&digest).unwrap(), "{label}: has_blob disagrees");
        assert_eq!(
            a.get_blob(&digest).expect("blob a"),
            b.get_blob(&digest).expect("blob b"),
            "{label}: blob bytes disagree"
        );
    }
}

#[test]
fn rekey_composite_ancestor_interleaved_converges_live_and_after_reopen() {
    // Reference oracle.
    let mut mem = MemoryStoreFactory::new()
        .create(InstanceId::new("rekey-ancestor"))
        .expect("mem create");
    let mem_blobs = apply_workload(&mut mem);

    // Backend under test.
    let handle = support::acquire();
    let mut factory = handle.factory("rekey_anc");
    let instance = InstanceId::new("rekey-ancestor");
    let _schema = SchemaGuard::new(&factory, instance.clone());

    let pg_blobs = {
        let mut pg = factory.create(instance.clone()).expect("pg create");
        let blobs = apply_workload(&mut pg);
        // Live: the pg projection must equal the reference op-for-op.
        assert_stores_agree(&mem, &pg, blobs, "live");
        blobs
        // `pg` drops here, closing the connection.
    };
    assert_eq!(mem_blobs, pg_blobs, "both backends compute the same blob digests");

    // Reopen from the durable tables alone, then re-compare the entire surface.
    let reopened = factory.reopen(instance.clone()).expect("reopen");
    assert_stores_agree(&mem, &reopened, pg_blobs, "reopened");
}
