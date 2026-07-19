//! RED TEAM — STRUCT keys under the tombstone / auto-ancestor / orphan machinery,
//! plus history points, blobs, and definition/composition with `U+0000`, checked
//! for a pg-vs-`MemoryStore` divergence live AND across a durable reopen.
//!
//! The existing struct-key case (`struct_key_divergence`) only drives TOP-LEVEL
//! struct keys through insert/rekey/scan/reopen. The tombstone red-teams
//! (`redteam_nested_without_ancestor`, `redteam_from_scratch_deep_autocreate_reopen`,
//! `redteam_composite_tombstone_reopen`) drive the tombstone / auto-created-ancestor
//! / orphan / rekey / revive machinery, but only over SCALAR and COMPOSITE keys —
//! never a `Value::Struct` key in an ancestor or leaf position. This case attacks
//! exactly that gap: every address level is a struct key (including a struct-in-struct
//! key), driven through the same adversarial edges the composite case names —
//! - a deep struct tombstone chain (mid-drop then top-drop leaves nested orphans),
//! - siblings inserted under multiply-tombstoned struct ancestors,
//! - a from-scratch deep chain whose struct ancestors were NEVER inserted
//!   (pg auto-creates them as struct tombstones; `key_enc`/`key_wire` for a struct
//!   ancestor must round-trip on reopen through `node_load`'s parent-chain walk),
//! - a rekey of a struct-keyed row whose ancestor is a struct tombstone,
//! - a revive / delete / revive cycle on a tombstoned struct row,
//!
//! and additionally exercises three surfaces the composite case leaves untouched:
//! - a struct ROW VALUE carrying a scale-bearing `decimal` and an omitted optional
//!   field, compared on the sharper A.7 canonical-JSON text axis (a lost decimal
//!   scale that Annex-B `Ord` equality hides would surface here),
//! - `record_point` history points at several frontiers (§19.3),
//! - definition source and a composition mount name/lineage/point carrying a
//!   `U+0000` (§19.1/§19.5) — the NUL-safe `text`/`jsonb` escape as a pg-vs-memory
//!   bijection through the real columns, never before compared for divergence.
//!
//! Cited: A.8 (struct key = key-eligible required fields), B.4 (a struct orders by
//! canonical FIELD-NAME order, distinct from a composite's positional order), B.5
//! (contiguous key-ascending scans), §5.4 (struct/nested key identity + logical
//! orphans), §21.1 (top-level drop leaves nested orphans), §19.1/§19.3/§19.5
//! (history points, definition, composition), §22.7/§19.2 (a reopen rebuilds an
//! identical projection; snapshots fold the durable log). Overarching gate: pg must
//! equal `MemoryStore` observably, on the Annex-B `Ord` axis AND the sharper A.7
//! canonical-text axis, and every ordering expectation is re-derived from B.4, never
//! from a backend's own answer.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_ident::{HistoryPoint, InstanceId, LineageId, NameSegment, PointId};
use liasse_store::{
    AddressStep, CollectionPath, CommitSeq, Composition, DefinitionText, InstanceStore, KeyValue,
    MemoryStoreFactory, Mount, RowAddress, StoreFactory, StoredRow, Transition,
};
use liasse_value::{Decimal, Integer, Struct, Text, Value};

fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}
fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// A two-field struct key `{r, c}` — fields declared r-then-c, but a [`Struct`]
/// holds them in canonical (BTreeMap) field-name order `c, r`, so B.4 orders by
/// `c` FIRST then `r`. A naive "r is primary" reading is wrong; the expected scan
/// order below is re-derived from that field-name rule.
fn grid_key(r: i64, c: i64) -> Value {
    Value::Struct(Struct::new([(Text::new("r"), int(r)), (Text::new("c"), int(c))]))
}
/// A single-field struct key `{n}` — distinct Value variant from a bare scalar
/// `n` (different B.4 rank + key_enc framing), so it round-trips as a struct.
fn cell_key(n: i64) -> Value {
    Value::Struct(Struct::new([(Text::new("n"), int(n))]))
}
/// A struct-in-struct key `{pos: {x, y}}` — a struct whose sole field's value is
/// itself a struct, exercising key_enc / key_wire recursion on the durable
/// ancestor path and its reopen reconstruction.
fn mark_key(x: i64, y: i64) -> Value {
    Value::Struct(Struct::new([(
        Text::new("pos"),
        Value::Struct(Struct::new([(Text::new("x"), int(x)), (Text::new("y"), int(y))])),
    )]))
}

fn grid_step(r: i64, c: i64) -> AddressStep {
    AddressStep::new(NameSegment::new("grids"), KeyValue::single(grid_key(r, c)))
}
fn cell_step(n: i64) -> AddressStep {
    AddressStep::new(NameSegment::new("cells"), KeyValue::single(cell_key(n)))
}
fn mark_step(x: i64, y: i64) -> AddressStep {
    AddressStep::new(NameSegment::new("marks"), KeyValue::single(mark_key(x, y)))
}

fn grid(r: i64, c: i64) -> RowAddress {
    RowAddress::root(grid_step(r, c))
}
fn cell(r: i64, c: i64, n: i64) -> RowAddress {
    grid(r, c).child(cell_step(n))
}
fn mark(r: i64, c: i64, n: i64, x: i64, y: i64) -> RowAddress {
    cell(r, c, n).child(mark_step(x, y))
}

// An isolated, insert-only top-level collection whose ONLY job is the externally
// derived B.4 field-name ordering claim (no deletes to muddy it).
fn plot_step(r: i64, c: i64) -> AddressStep {
    AddressStep::new(NameSegment::new("plots"), KeyValue::single(grid_key(r, c)))
}
fn plot(r: i64, c: i64) -> RowAddress {
    RowAddress::root(plot_step(r, c))
}

fn grids_collection() -> CollectionPath {
    CollectionPath::top(NameSegment::new("grids"))
}
fn plots_collection() -> CollectionPath {
    CollectionPath::top(NameSegment::new("plots"))
}
fn cells_collection(r: i64, c: i64) -> CollectionPath {
    CollectionPath::nested(grid(r, c).steps().cloned(), NameSegment::new("cells"))
}
fn marks_collection(r: i64, c: i64, n: i64) -> CollectionPath {
    CollectionPath::nested(cell(r, c, n).steps().cloned(), NameSegment::new("marks"))
}

/// A struct ROW VALUE carrying a `decimal` (input `1.50`, scale 2) and an omitted
/// optional (`none`) field alongside a nested struct. Since SPEC-ISSUES #1 the
/// decimal's canonical text is minimal scale (`1.5`), so the A.7 canonical-text
/// axis must render it identically — `1.5` — through the pg `jsonb` round-trip and
/// reopen; the axis still guards the `none`/nested-struct shape that Annex-B `Ord`
/// equality hides.
fn struct_value() -> Value {
    Value::Struct(Struct::new([
        (Text::new("ratio"), Value::Decimal(Decimal::parse("1.50").expect("decimal"))),
        (Text::new("absent"), Value::None),
        (
            Text::new("meta"),
            Value::Struct(Struct::new([(Text::new("a"), int(1)), (Text::new("b"), text("z"))])),
        ),
    ]))
}

/// The identical adversarial op stream both backends run. Each `begin/commit` pair
/// is one committed transition; nothing here depends on a backend's own answer.
fn apply_workload<S: InstanceStore>(store: &mut S) {
    // 0. Insert-only `plots` in scrambled order — for the standalone B.4 ordering
    //    check. Payload tags name each row so the scan sequence is externally read.
    let mut txn = store.begin();
    txn.insert(plot(1, 2), text("p-r1c2")).unwrap();
    txn.insert(plot(2, 1), text("p-r2c1")).unwrap();
    txn.insert(plot(2, 2), text("p-r2c2")).unwrap();
    txn.insert(plot(1, 1), text("p-r1c1")).unwrap();
    txn.commit().unwrap();

    // 1. Seed a struct-keyed tree three levels deep under grid(1,1).
    let mut txn = store.begin();
    txn.insert(grid(1, 1), text("g-1-1")).unwrap();
    txn.insert(grid(2, 2), struct_value()).unwrap(); // struct VALUE at a struct-keyed row
    txn.commit().unwrap();

    let mut txn = store.begin();
    txn.insert(cell(1, 1, 10), text("c-10")).unwrap();
    txn.insert(cell(1, 1, 20), text("c-20")).unwrap();
    txn.commit().unwrap();

    let mut txn = store.begin();
    txn.insert(mark(1, 1, 10, 1, 1), text("m-1-1")).unwrap();
    txn.insert(mark(1, 1, 10, 1, 2), struct_value()).unwrap(); // struct VALUE at a struct-in-struct key
    txn.commit().unwrap();

    // 2. Delete the MID struct node cell(1,1,10): its marks become orphans under a
    //    struct tombstone.
    let mut txn = store.begin();
    txn.delete(&cell(1, 1, 10)).unwrap();
    txn.commit().unwrap();

    // 3. Delete the TOP struct node grid(1,1): the whole subtree is orphaned under
    //    struct tombstones (grid tombstone -> cell tombstone -> marks).
    let mut txn = store.begin();
    txn.delete(&grid(1, 1)).unwrap();
    txn.commit().unwrap();

    // 4. Insert a NEW sibling cell directly under the tombstoned struct grid(1,1).
    let mut txn = store.begin();
    txn.insert(cell(1, 1, 30), text("c-30")).unwrap();
    txn.commit().unwrap();

    // 5. Insert a NEW mark under DOUBLY-tombstoned struct ancestors
    //    (grid(1,1) tombstone -> cell(1,1,10) tombstone -> new mark).
    let mut txn = store.begin();
    txn.insert(mark(1, 1, 10, 9, 9), text("m-9-9")).unwrap();
    txn.commit().unwrap();

    // 6. Rekey a mark whose ancestors are struct tombstones to a new struct-in-struct
    //    key — the moved leaf, its source tombstoned, descendants (none) untouched.
    let mut txn = store.begin();
    txn.rekey(&mark(1, 1, 10, 1, 1), mark(1, 1, 10, 5, 5), text("m-5-5")).unwrap();
    txn.commit().unwrap();

    // 7. From-scratch DEEP struct chain: a mark under grid(7,7)/cell(7,7,77) where
    //    NEITHER ancestor was ever inserted — pg auto-creates BOTH as struct
    //    tombstones; memory admits the orphan directly. Reopen must reconstruct the
    //    mark's address through the auto-created struct tombstones.
    let mut txn = store.begin();
    txn.insert(mark(7, 7, 77, 3, 4), text("m-fromscratch")).unwrap();
    txn.commit().unwrap();

    // 8. Revive / delete / revive cycle on the tombstoned struct grid(1,1).
    let mut txn = store.begin();
    txn.insert(grid(1, 1), text("g-1-1-revived")).unwrap();
    txn.commit().unwrap();
    let mut txn = store.begin();
    txn.delete(&grid(1, 1)).unwrap();
    txn.commit().unwrap();
    let mut txn = store.begin();
    txn.insert(grid(1, 1), text("g-1-1-revived-again")).unwrap();
    txn.commit().unwrap();

    // 9. Set the active definition + composition, with a `U+0000` woven through the
    //    definition source and the composition mount name / lineage / point — the
    //    NUL-safe text/jsonb escape as a bijection through the real columns.
    let mut txn = store.begin();
    txn.set_definition(DefinitionText::new("{\"$app\":\"t\\u0000\",\"src\":\"a\u{0}b\"}"));
    let composition = Composition::new().with(
        "child\u{0}mount",
        Mount::new(
            InstanceId::new("inst\u{0}9"),
            HistoryPoint::new(LineageId::new("lin\u{0}eage"), PointId::new("po\u{0}int")),
        ),
    );
    txn.set_composition(composition);
    txn.commit().unwrap();

    // 10. Blobs (§18 store surface): content-addressed put; one carries a NUL byte.
    store.put_blob(b"struct-key-blob-payload").expect("put blob");
    store.put_blob(&[0u8, 1, 2, 0, 255]).expect("put blob with NUL bytes");

    // 11. History points at the current head and at an early frontier (§19.3).
    let head = store.head().unwrap();
    store
        .record_point(head, HistoryPoint::new(LineageId::new("main"), PointId::new("head")))
        .expect("record head point");
    store
        .record_point(
            CommitSeq::from_stored(3),
            HistoryPoint::new(LineageId::new("main"), PointId::new("seed")),
        )
        .expect("record seed point");
}

/// Every address the workload touches — present or tombstoned — so both presence and
/// absence are compared at each.
fn touched() -> Vec<RowAddress> {
    vec![
        plot(1, 1),
        plot(1, 2),
        plot(2, 1),
        plot(2, 2),
        grid(1, 1),
        grid(2, 2),
        cell(1, 1, 10),
        cell(1, 1, 20),
        cell(1, 1, 30),
        cell(7, 7, 77),
        mark(1, 1, 10, 1, 1),
        mark(1, 1, 10, 1, 2),
        mark(1, 1, 10, 5, 5),
        mark(1, 1, 10, 9, 9),
        mark(7, 7, 77, 3, 4),
    ]
}

fn collections() -> Vec<CollectionPath> {
    vec![
        plots_collection(),
        grids_collection(),
        cells_collection(1, 1),
        cells_collection(2, 2),
        cells_collection(7, 7),
        marks_collection(1, 1, 10),
        marks_collection(1, 1, 20),
        marks_collection(7, 7, 77),
    ]
}

/// The blob payloads the workload stores, recomputed independently so `has_blob`
/// can be checked without depending on a store's own answer.
fn blob_payloads() -> Vec<Vec<u8>> {
    vec![b"struct-key-blob-payload".to_vec(), vec![0u8, 1, 2, 0, 255]]
}

/// The history points the workload records at a fixed early frontier.
fn recorded_points() -> Vec<(HistoryPoint, CommitSeq)> {
    vec![(HistoryPoint::new(LineageId::new("main"), PointId::new("seed")), CommitSeq::from_stored(3))]
}

fn sorted_scan<S: InstanceStore>(store: &S, c: &CollectionPath) -> Vec<(RowAddress, StoredRow)> {
    let mut rows = store.scan(c).expect("scan");
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// The canonical A.7 text of every row payload in a scan, in scan order — the
/// sharper axis that distinguishes a lost `decimal` scale / struct shape that
/// Annex-B `Ord` equality collapses.
fn scan_canonical_text<S: InstanceStore>(store: &S, c: &CollectionPath) -> Vec<String> {
    sorted_scan(store, c)
        .into_iter()
        .map(|(_, row)| row.value().to_canonical_json_string())
        .collect()
}

/// Assert two stores are observably identical: head, every touched row (presence AND
/// absence, incarnation + canonical-text included), every collection scan (order +
/// canonical text), every frontier snapshot, the whole commit log, every recorded
/// history point, every stored blob, and the active definition + composition.
fn assert_stores_agree<A: InstanceStore, B: InstanceStore>(a: &A, b: &B, label: &str) {
    assert_eq!(a.head().unwrap(), b.head().unwrap(), "{label}: head disagrees");

    for address in touched() {
        let ra = a.row(&address).expect("row a");
        let rb = b.row(&address).expect("row b");
        assert_eq!(ra, rb, "{label}: row disagrees at {}", address.render());
        // Sharper canonical-text axis for present rows (scale/structure preserving).
        if let (Some(x), Some(y)) = (&ra, &rb) {
            assert_eq!(
                x.value().to_canonical_json_string(),
                y.value().to_canonical_json_string(),
                "{label}: row CANONICAL-TEXT disagrees at {}",
                address.render()
            );
        }
    }

    for c in collections() {
        assert_eq!(sorted_scan(a, &c), sorted_scan(b, &c), "{label}: scan disagrees for a collection");
        assert_eq!(
            scan_canonical_text(a, &c),
            scan_canonical_text(b, &c),
            "{label}: scan CANONICAL-TEXT disagrees for a collection"
        );
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

    // Full commit log replays identically (length + ops).
    let la = a.log_from(CommitSeq::GENESIS).expect("log a");
    let lb = b.log_from(CommitSeq::GENESIS).expect("log b");
    assert_eq!(la, lb, "{label}: commit log disagrees");

    // History points: seed at frontier 3, and head at the current head.
    for (point, at) in recorded_points() {
        assert_eq!(a.point_position(&point).unwrap(), Some(at), "{label}: point_position a");
        assert_eq!(b.point_position(&point).unwrap(), Some(at), "{label}: point_position b");
    }
    let head_point = HistoryPoint::new(LineageId::new("main"), PointId::new("head"));
    assert_eq!(
        a.point_position(&head_point).unwrap(),
        b.point_position(&head_point).unwrap(),
        "{label}: head point_position disagrees"
    );

    // Blobs (§18): both hold every payload; a NUL byte in the bytes must survive.
    for payload in blob_payloads() {
        let digest = {
            use sha2::{Digest as _, Sha512};
            let mut h = Sha512::new();
            h.update(&payload);
            liasse_value::Sha512::parse(&data_encoding::HEXLOWER.encode(&h.finalize()))
                .expect("digest")
        };
        assert!(a.has_blob(&digest).unwrap(), "{label}: blob missing from a");
        assert!(b.has_blob(&digest).unwrap(), "{label}: blob missing from b");
        assert_eq!(a.get_blob(&digest).unwrap(), Some(payload.clone()), "{label}: blob bytes a");
        assert_eq!(b.get_blob(&digest).unwrap(), Some(payload), "{label}: blob bytes b");
    }

    // Definition + composition (§19.1/§19.5), with a `U+0000` woven through both.
    assert_eq!(a.definition().unwrap(), b.definition().unwrap(), "{label}: definition disagrees");
    assert_eq!(a.composition().unwrap(), b.composition().unwrap(), "{label}: composition disagrees");
}

/// The externally-derived B.4 field-name scan order of `plots`: fields `{r, c}`
/// canonicalize to `c, r`, so ordering is by `c` FIRST then `r`. Payload tags name
/// each row, so the sequence is checkable against the spec, not a backend's answer.
fn expected_plot_order() -> Vec<Value> {
    vec![
        text("p-r1c1"), // c=1,r=1
        text("p-r2c1"), // c=1,r=2
        text("p-r1c2"), // c=2,r=1
        text("p-r2c2"), // c=2,r=2
    ]
}

#[test]
fn struct_key_tombstone_tree_is_zero_divergence_across_reopen() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("structkeytombstone");
    let instance = InstanceId::new("struct-key-tombstone-reopen");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    // Oracle and pg run the identical workload, so their opaque `row-N` incarnations
    // line up op-for-op.
    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory");
    apply_workload(&mut memory);

    // The oracle's own `plots` order must be the spec's B.4 field-name order, so the
    // mem-vs-pg agreement below is not two backends sharing one wrong answer.
    let mem_plots: Vec<Value> =
        sorted_scan(&memory, &plots_collection()).into_iter().map(|(_, r)| r.value().clone()).collect();
    assert_eq!(mem_plots, expected_plot_order(), "the reference orders a struct key by B.4 field-name");

    let mut pg = pg_factory.create(instance.clone()).expect("create pg");
    apply_workload(&mut pg);

    // Live: pg projection matches the oracle exactly, on both axes.
    assert_stores_agree(&pg, &memory, "live pg vs memory");
    let pg_plots: Vec<Value> =
        sorted_scan(&pg, &plots_collection()).into_iter().map(|(_, r)| r.value().clone()).collect();
    assert_eq!(pg_plots, expected_plot_order(), "live pg orders a struct key by B.4 field-name");

    // Reopen from the durable node tree — the process-restart path — and re-compare.
    // A struct address that fails to reconstruct through a tombstoned/auto-created
    // struct ancestor, a lost/duplicated orphan, or a lost struct-value scale would
    // surface here.
    let reopened = pg_factory.reopen(instance).expect("reopen pg");
    assert_stores_agree(&reopened, &memory, "reopened pg vs memory");
    let reopened_plots: Vec<Value> = sorted_scan(&reopened, &plots_collection())
        .into_iter()
        .map(|(_, r)| r.value().clone())
        .collect();
    assert_eq!(reopened_plots, expected_plot_order(), "reopened pg keeps B.4 field-name order");
}
