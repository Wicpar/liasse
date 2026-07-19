//! RED TEAM — the FROM-SCRATCH multi-level auto-create-ancestor-as-tombstone path
//! (`node_write::resolve_or_create` / `create_tombstone`, landed 259ac69/09d9ed6),
//! checked for a pg-vs-`MemoryStore` divergence live AND across a durable reopen.
//!
//! The two existing tombstone guards leave the sharpest edge of this fix untested:
//!
//!   - `redteam_nested_without_ancestor` auto-creates exactly ONE ancestor
//!     tombstone (a 2-level `/orgs/1/teams/10` with `/orgs/1` absent) and only
//!     asserts `row()` equality — never a scan, snapshot, log, or reopen.
//!   - `redteam_composite_tombstone_reopen` always SEEDS its ancestors live first,
//!     so every later nested op resolves its parent from an ancestor already in
//!     `by_id` (live, or delete-tombstoned but retained). `create_tombstone`'s
//!     from-scratch recursion is therefore barely exercised there.
//!
//! This case drives the untested core directly: a single `insert(/n1/n2/n3/n4)`
//! whose ENTIRE ancestor chain was never a node forces `resolve_or_create` to
//! materialize THREE tombstones in one recursive descent, and it does so inside a
//! commit that also carries ordinary inserts and further from-scratch deep inserts
//! — the exact shape that would expose a positional `new_ids` replay desync if
//! `create_tombstone` ever leaked an id into `new_ids` (a tombstone establishes no
//! live address; the projection advances `by_id` one entry per real `Insert`/`Rekey`
//! in op order, so a leaked tombstone id would mis-map a later live address and
//! corrupt a subsequent nested/rekey resolution, surfacing after a reopen).
//!
//! It then walks every adjacent edge the fix must honor: revive an auto-created
//! tombstone (it becomes a live row while deeper tombstones stay absent as rows and
//! the deep leaf stays live); revive a mid then DELETE it (re-tombstone a live mid
//! that has both a tombstone and a live descendant) and insert a new sibling under
//! the re-tombstoned mid; rekey the live leaf under an auto-created tombstone; rekey
//! a deep row to a fresh from-scratch deep TARGET (whose own ancestors auto-create);
//! and a COMPOSITE-keyed from-scratch deep chain. Interleaved throughout: a blob, a
//! definition, a composition, and a recorded history point — the full store-contract
//! battery folded through auto-created tombstones.
//!
//! Every expectation is the `MemoryStore` oracle running the identical op stream —
//! never the pg backend's own answer. The store contract is semantics-free and pins
//! no ancestor precondition (§5.4: a nested row carries its own identity plus
//! ancestor identity; a logical orphan is an addressable descendant of an absent
//! ancestor), so both backends MUST agree observably.
//!
//! Cited: §5.4 (nested/orphan row identity, no ancestor precondition), §21.1 (a
//! drop leaves nested orphans), B.4/B.5 (composite ordering, key-ascending scans),
//! §19.2/§22.7 (a reopen rebuilds an identical projection; snapshots fold the
//! durable log), §18/§19.1/§19.5 (blobs, definition, composition, history points).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_ident::{HistoryPoint, InstanceId, LineageId, NameSegment, PointId};
use liasse_store::{
    AddressStep, CollectionPath, CommitSeq, Composition, DefinitionText, InstanceStore, KeyValue,
    MemoryStoreFactory, Mount, RowAddress, StoreFactory, StoredRow, Transition,
};
use liasse_value::{Integer, Sha512, Text, Value};

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}
fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}

// ---- int-keyed 4-level hierarchy: n1 > n2 > n3 > n4 ------------------------

fn int_step(name: &str, k: i64) -> AddressStep {
    AddressStep::new(NameSegment::new(name), KeyValue::single(int(k)))
}
fn n1(a: i64) -> RowAddress {
    RowAddress::root(int_step("n1", a))
}
fn n2(a: i64, b: i64) -> RowAddress {
    n1(a).child(int_step("n2", b))
}
fn n3(a: i64, b: i64, c: i64) -> RowAddress {
    n2(a, b).child(int_step("n3", c))
}
fn n4(a: i64, b: i64, c: i64, d: i64) -> RowAddress {
    n3(a, b, c).child(int_step("n4", d))
}

// ---- composite-keyed 3-level hierarchy: corg > cteam > cmember -------------

fn comp_step(name: &str, a: &str, b: i64) -> AddressStep {
    AddressStep::new(NameSegment::new(name), KeyValue::composite(text(a), vec![int(b)]))
}
fn corg(region: &str, code: i64) -> RowAddress {
    RowAddress::root(comp_step("corg", region, code))
}
fn cteam(region: &str, code: i64, dept: &str, num: i64) -> RowAddress {
    corg(region, code).child(comp_step("cteam", dept, num))
}
fn cmember(region: &str, code: i64, dept: &str, num: i64, m: i64) -> RowAddress {
    cteam(region, code, dept, num).child(int_step("cmember", m))
}

fn collections() -> Vec<CollectionPath> {
    vec![
        CollectionPath::top(NameSegment::new("n1")),
        CollectionPath::nested(n1(1).steps().cloned(), NameSegment::new("n2")),
        CollectionPath::nested(n2(1, 2).steps().cloned(), NameSegment::new("n3")),
        CollectionPath::nested(n3(1, 2, 3).steps().cloned(), NameSegment::new("n4")),
        // The rekey target's fresh from-scratch chain.
        CollectionPath::nested(n1(11).steps().cloned(), NameSegment::new("n2")),
        CollectionPath::nested(n2(11, 12).steps().cloned(), NameSegment::new("n3")),
        // Composite chain.
        CollectionPath::top(NameSegment::new("corg")),
        CollectionPath::nested(corg("eu", 1).steps().cloned(), NameSegment::new("cteam")),
        CollectionPath::nested(
            cteam("eu", 1, "sales", 10).steps().cloned(),
            NameSegment::new("cmember"),
        ),
    ]
}

/// Every address the workload touches — live rows, addresses that are only ever
/// auto-created tombstones (must read back ABSENT), revived/re-deleted mids, rekey
/// sources/targets — so BOTH presence and absence are compared at each. A tombstone
/// is a structural position, never an observable row (§5.4), so an auto-created
/// ancestor must be absent from `row()` on both backends.
fn touched() -> Vec<RowAddress> {
    vec![
        // The first from-scratch deep chain: n1[1]/n2[1,2]/n3[1,2,3] are auto-created
        // tombstones; n4[1,2,3,4] is the live leaf.
        n1(1),
        n2(1, 2),
        n3(1, 2, 3),
        n4(1, 2, 3, 4),
        n4(1, 2, 3, 9),  // sibling inserted under the re-tombstoned n2[1,2]
        n4(1, 2, 3, 10), // rekey target of n4[1,2,3,4]
        // Ordinary top-level + second from-scratch deep chain from commit 1.
        n1(5),
        n1(6),
        n2(6, 7),
        n3(6, 7, 8),
        // Rekey-of-deep target chain (auto-creates n1[11], n2[11,12]).
        n1(11),
        n2(11, 12),
        n3(11, 12, 13),
        // Composite from-scratch chain.
        corg("eu", 1),
        cteam("eu", 1, "sales", 10),
        cmember("eu", 1, "sales", 10, 100),
    ]
}

fn point() -> HistoryPoint {
    HistoryPoint::new(LineageId::new("main"), PointId::new("p1"))
}

fn composition() -> Composition {
    Composition::new().with(
        "child",
        Mount::new(InstanceId::new("child-inst"), HistoryPoint::new(LineageId::new("main"), PointId::new("cp"))),
    )
}

/// The identical adversarial op stream both backends run. Each `begin/commit` pair
/// is one committed transition, and every allocated `row-N` incarnation lines up
/// op-for-op because the two backends stage the identical inserts/rekeys in order.
const BLOB_BYTES: &[u8] = b"a durable blob for the fold";

fn apply_workload<S: InstanceStore>(store: &mut S) -> Sha512 {
    // Definition + a blob before any rows (durable metadata folded alongside).
    let digest = store.put_blob(BLOB_BYTES).unwrap();
    let mut txn = store.begin();
    txn.set_definition(DefinitionText::new("{\"$liasse\":1,\"$app\":\"t@1.0.0\"}"));
    txn.set_composition(composition());
    txn.insert(n1(0), text("anchor")).unwrap(); // one ordinary row so this commit is non-empty
    txn.commit().unwrap();

    // COMMIT 1 — the core probe: a from-scratch 4-deep insert (auto-creates THREE
    // ancestor tombstones in one recursive descent) sits in the SAME commit as an
    // ordinary top-level insert AND a second from-scratch 3-deep insert. If any
    // auto-created tombstone leaked into `new_ids`, the projection's positional
    // replay would mis-map n1[5] or n3[6,7,8] here — corrupting a later resolution.
    let mut txn = store.begin();
    txn.insert(n4(1, 2, 3, 4), text("leaf-d4")).unwrap();
    txn.insert(n1(5), text("top-5")).unwrap();
    txn.insert(n3(6, 7, 8), text("deep-678")).unwrap();
    txn.commit().unwrap();

    // COMMIT 2 — revive an auto-created tombstone chain top-down: n1[1] then n2[1,2]
    // become live rows while n3[1,2,3] stays a tombstone and n4[1,2,3,4] stays live.
    let mut txn = store.begin();
    txn.insert(n1(1), text("revived-n1")).unwrap();
    txn.insert(n2(1, 2), text("revived-n2")).unwrap();
    txn.commit().unwrap();

    // COMMIT 3 — DELETE a revived mid that has BOTH a tombstone descendant (n3) and a
    // live descendant (n4): it re-tombstones. Then insert a NEW sibling leaf under the
    // re-tombstoned mid (through the still-tombstoned n3[1,2,3]).
    let mut txn = store.begin();
    txn.delete(&n2(1, 2)).unwrap();
    txn.insert(n4(1, 2, 3, 9), text("sibling-d9")).unwrap();
    txn.commit().unwrap();

    // COMMIT 4 — rekey the live leaf under the auto-created tombstone n3[1,2,3]
    // (same-parent leaf move), and rekey a deep row to a FRESH from-scratch deep
    // target whose own ancestors (n1[11], n2[11,12]) must auto-create as tombstones.
    let mut txn = store.begin();
    txn.rekey(&n4(1, 2, 3, 4), n4(1, 2, 3, 10), text("moved-d10")).unwrap();
    txn.rekey(&n3(6, 7, 8), n3(11, 12, 13), text("moved-deep")).unwrap();
    txn.commit().unwrap();

    // COMMIT 5 — a COMPOSITE-keyed from-scratch deep chain: a member with NO composite
    // ancestors auto-creates corg[eu,1] and cteam[sales,10] as composite tombstones;
    // then revive the composite org so it becomes a live row over its tombstone.
    let mut txn = store.begin();
    txn.insert(cmember("eu", 1, "sales", 10, 100), text("cm-100")).unwrap();
    txn.insert(corg("eu", 1), text("corg-live")).unwrap();
    txn.commit().unwrap();

    // Record a history point at the current head — durable, folded on reopen.
    let head = store.head().unwrap();
    store.record_point(head, point()).unwrap();
    digest
}

fn sorted_scan<S: InstanceStore>(store: &S, c: &CollectionPath) -> Vec<(RowAddress, StoredRow)> {
    let mut rows = store.scan(c).expect("scan");
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// The A.7 canonical-JSON text of a stored value — the sharper axis that catches a
/// scale/precision/structure loss `Value::Eq` (Annex-B `Ord`) would hide.
fn ctext(row: &StoredRow) -> String {
    row.value().to_canonical_json_string()
}

/// Assert two stores are observably identical on the WHOLE contract surface: head,
/// every touched row (presence, absence, incarnation, and canonical text), every
/// collection scan (order included), every frontier snapshot, the whole commit log,
/// the blob, the definition, the composition, and the recorded history point.
fn assert_stores_agree<A: InstanceStore, B: InstanceStore>(a: &A, b: &B, digest: &Sha512, label: &str) {
    assert_eq!(a.head().unwrap(), b.head().unwrap(), "{label}: head disagrees");

    for address in touched() {
        let ra = a.row(&address).expect("row a");
        let rb = b.row(&address).expect("row b");
        assert_eq!(ra, rb, "{label}: row disagrees at {}", address.render());
        // Sharper canonical-text axis where both are present.
        if let (Some(ra), Some(rb)) = (&ra, &rb) {
            assert_eq!(
                ctext(ra),
                ctext(rb),
                "{label}: canonical-text disagrees at {}",
                address.render()
            );
        }
    }

    for c in collections() {
        let sa = sorted_scan(a, &c);
        let sb = sorted_scan(b, &c);
        assert_eq!(sa, sb, "{label}: scan disagrees for a collection");
        for ((_, x), (_, y)) in sa.iter().zip(sb.iter()) {
            assert_eq!(ctext(x), ctext(y), "{label}: scan canonical-text disagrees");
        }
    }

    // Snapshot at every frontier from genesis to head folds the durable log.
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

    // The full commit log must replay identically (length and per-transition seq/ops).
    let la = a.log_from(CommitSeq::GENESIS).expect("log a");
    let lb = b.log_from(CommitSeq::GENESIS).expect("log b");
    assert_eq!(la.len(), lb.len(), "{label}: log length disagrees");
    for (ta, tb) in la.iter().zip(lb.iter()) {
        assert_eq!(ta.seq(), tb.seq(), "{label}: log seq disagrees");
        assert_eq!(ta.ops(), tb.ops(), "{label}: log ops disagree at seq {}", ta.seq().get());
    }

    // Durable metadata.
    assert_eq!(a.definition().unwrap(), b.definition().unwrap(), "{label}: definition disagrees");
    assert_eq!(a.composition().unwrap(), b.composition().unwrap(), "{label}: composition disagrees");
    assert_eq!(
        a.point_position(&point()).unwrap(),
        b.point_position(&point()).unwrap(),
        "{label}: history point position disagrees"
    );
    assert!(a.has_blob(digest).unwrap() && b.has_blob(digest).unwrap(), "{label}: blob presence disagrees");
    assert_eq!(
        a.get_blob(digest).expect("blob a"),
        b.get_blob(digest).expect("blob b"),
        "{label}: blob bytes disagree"
    );
    assert_eq!(a.get_blob(digest).expect("blob a"), Some(BLOB_BYTES.to_vec()), "{label}: blob bytes lost");
}

/// The externally-known oracle itself must show every auto-created ancestor as an
/// ABSENT row and every live/revived row present — so the pg comparison is against a
/// verified truth, not a mutual echo.
fn assert_oracle_shape<S: InstanceStore>(store: &S) {
    // Auto-created-only ancestors that were never explicitly inserted, and the
    // re-deleted mid, must be absent.
    for absent in [n3(1, 2, 3), n1(6), n2(6, 7), n1(11), n2(11, 12), n2(1, 2), cteam("eu", 1, "sales", 10)] {
        assert!(
            store.row(&absent).expect("row").is_none(),
            "oracle: {} must be absent (tombstone/auto-created or re-deleted)",
            absent.render()
        );
    }
    // Live rows: the revived top, the moved leaf, the sibling, the composite leaf/org.
    for (present, want) in [
        (n1(1), "revived-n1"),
        (n4(1, 2, 3, 10), "moved-d10"),
        (n4(1, 2, 3, 9), "sibling-d9"),
        (n3(11, 12, 13), "moved-deep"),
        (cmember("eu", 1, "sales", 10, 100), "cm-100"),
        (corg("eu", 1), "corg-live"),
    ] {
        let row = store.row(&present).expect("row").unwrap_or_else(|| panic!("{} must be present", present.render()));
        assert_eq!(row.value(), &text(want), "oracle: value at {}", present.render());
    }
    // The original leaf address is now vacated by the rekey.
    assert!(store.row(&n4(1, 2, 3, 4)).expect("row").is_none(), "oracle: rekey source must be vacated");
    assert!(store.row(&n3(6, 7, 8)).expect("row").is_none(), "oracle: rekey source (deep) must be vacated");
}

#[test]
fn from_scratch_deep_autocreate_is_zero_divergence_across_reopen() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("fromscratchdeep");
    let instance = InstanceId::new("from-scratch-deep-autocreate");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    // Reference oracle: MemoryStore runs the identical op stream and holds the
    // logical-orphan state verbatim (a flat address->row map, no tombstones).
    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory");
    let digest = apply_workload(&mut memory);
    assert_oracle_shape(&memory);

    // Backend under test.
    let mut pg = pg_factory.create(instance.clone()).expect("create pg");
    let pg_digest = apply_workload(&mut pg);
    assert_eq!(digest, pg_digest, "content-addressed blob digests must match across backends");

    // Live: pg equals the oracle on the whole surface.
    assert_oracle_shape(&pg);
    assert_stores_agree(&pg, &memory, &digest, "live pg vs memory");

    // Reopen: the entire read model is rebuilt purely from the durable `nodes` tree,
    // walking each address through its (possibly auto-created, possibly re-tombstoned)
    // ancestor chain. This is where a from-scratch multi-level auto-create that laid a
    // wrong parent link, leaked a tombstone as a row, or desynced the `new_ids` replay
    // would first surface.
    let reopened = pg_factory.reopen(instance).expect("reopen pg");
    assert_oracle_shape(&reopened);
    assert_stores_agree(&reopened, &memory, &digest, "reopened pg vs memory");
}
