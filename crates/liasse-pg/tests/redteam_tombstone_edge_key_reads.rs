//! RED TEAM — the chained-InitPlan point lookup (`crate::read`, §4.1/§4.2) must
//! resolve a deep address by hopping `(parent_id, step_name, key_enc)` THROUGH a
//! tombstoned intermediate ancestor whose key carries EDGE BYTES, and must match
//! the reference store's Annex-B resolution exactly (SPEC-ISSUES item 32: a backend
//! disagreement is always a fix).
//!
//! Two hazards meet here that the corpus does not combine:
//!
//!   1. an INTERMEDIATE hop that is a **tombstone** — the `parent_chain` scalar
//!      subquery must NOT filter `value IS NOT NULL`, or the walk to a live orphan
//!      under a deleted ancestor fail-closes to `None` (§5.4, a MEDIUM false reject);
//!   2. an intermediate key whose `key_enc` bytes are **edge cases**: an interior
//!      `U+0000` in `text`, an interior `0x00` in `bytes`, the empty `text` and
//!      empty `bytes`, `i64::MAX`, and — sharpest — a **scale-variant `decimal`**
//!      read (`1.5`) against a row inserted at `1.500`, which only resolves if the
//!      intermediate hop canonicalizes `key_enc` the same way at read and write.
//!
//! For every edge key E the workload inserts `/mid/E` and the nested `/mid/E/leaf/1`,
//! then DELETES `/mid/E` (tombstoning the edge-keyed intermediate) and reads the
//! orphan `/mid/E/leaf/1` back — through the tombstone — comparing `row` and `scan`
//! against the in-memory reference, live and across a durable reopen.
//!
//! A final probe drives DEGENERATE / DEEP addresses (a depth-8 insert+read, a
//! never-existed deep read, an empty scan) to confirm the read path errors or
//! returns cleanly — never panics or hangs (§11 no-panic).
//!
//! Resolves the DSN through [`support`] and drops its throwaway schema through a
//! [`support::SchemaGuard`] even on a panic.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress,
    StoreFactory, StoredRow, Transition,
};
use liasse_value::{Bytes, Decimal, Integer, Text, Value};

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
fn mid(key: KeyValue) -> RowAddress {
    addr(&[("mid", key)])
}
fn leaf(key: KeyValue) -> RowAddress {
    addr(&[("mid", key), ("leaf", KeyValue::single(Value::Int(Integer::from(1))))])
}

/// The edge-byte intermediate keys the tombstone-through read must survive.
fn edge_keys() -> Vec<(&'static str, KeyValue)> {
    vec![
        ("nul-text", KeyValue::single(text("x\u{0}y"))),
        ("empty-text", KeyValue::single(text(""))),
        ("nul-bytes", KeyValue::single(Value::Bytes(Bytes::new(vec![0u8, 1, 0, 2])))),
        ("empty-bytes", KeyValue::single(Value::Bytes(Bytes::new(Vec::new())))),
        ("max-int", KeyValue::single(Value::Int(Integer::from(i64::MAX)))),
        ("min-int", KeyValue::single(Value::Int(Integer::from(i64::MIN)))),
        (
            "decimal-scaled",
            KeyValue::single(Value::Decimal(Decimal::parse("1.500").expect("decimal"))),
        ),
    ]
}

/// Insert `/mid/E` + `/mid/E/leaf/1` for every edge key, then tombstone every
/// `/mid/E` so each leaf becomes an orphan under a tombstoned edge-keyed ancestor.
fn build<S: InstanceStore>(store: &mut S) {
    for (_, key) in edge_keys() {
        let mut txn = store.begin();
        txn.insert(mid(key.clone()), text("mid-row")).expect("insert mid");
        txn.insert(leaf(key.clone()), text("leaf-row")).expect("insert leaf");
        txn.commit().expect("commit pair");
    }
    for (_, key) in edge_keys() {
        let mut txn = store.begin();
        txn.delete(&mid(key)).expect("delete mid (tombstone)");
        txn.commit().expect("commit tombstone");
    }
}

/// Compare `row(leaf)` (through the tombstoned edge-keyed ancestor), `row(mid)`
/// (must be `None` — a tombstone is not a row), and the ordered `scan` of each
/// leaf collection, between the reference and the pg store.
fn assert_edges_match<A: InstanceStore, B: InstanceStore>(memory: &A, pg: &B, label: &str) {
    for (name, key) in edge_keys() {
        // The tombstoned intermediate is not a row on either backend.
        let mm = memory.row(&mid(key.clone())).expect("mem mid");
        let pm = pg.row(&mid(key.clone())).expect("pg mid");
        assert_eq!(mm, pm, "{label}/{name}: tombstoned mid must read identically (both None)");
        assert!(pm.is_none(), "{label}/{name}: a tombstone is not a row");

        // The orphan leaf resolves THROUGH the tombstone on both.
        let ml = memory.row(&leaf(key.clone())).expect("mem leaf");
        let pl = pg.row(&leaf(key.clone())).expect("pg leaf");
        assert_eq!(
            ml, pl,
            "{label}/{name}: orphan leaf under a tombstoned edge-keyed ancestor diverged — \
             memory={ml:?} pg={pl:?}"
        );
        assert!(pl.is_some(), "{label}/{name}: the orphan leaf must still be readable (§5.4)");

        // The nested scan reaches the orphan through the tombstoned intermediate.
        let coll = CollectionPath::nested(mid(key.clone()).steps().cloned(), NameSegment::new("leaf"));
        let ms: Vec<(RowAddress, StoredRow)> = memory.scan(&coll).expect("mem scan leaf");
        let ps: Vec<(RowAddress, StoredRow)> = pg.scan(&coll).expect("pg scan leaf");
        assert_eq!(ms, ps, "{label}/{name}: nested orphan scan diverged");
        assert_eq!(ps.len(), 1, "{label}/{name}: exactly the one orphan leaf");
    }
}

#[test]
fn tombstoned_edge_key_ancestor_reads_match_memory() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("tombedge");
    let instance = InstanceId::new("tombstone-edge-key-reads");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");
    build(&mut memory);
    build(&mut pg);

    assert_edges_match(&memory, &pg, "live");

    // The sharpest hop: read the orphan via a SCALE-VARIANT decimal key (`1.5`)
    // against the row inserted+tombstoned at `1.500`. Both backends must canonicalize
    // the intermediate key to the same identity and resolve the same orphan.
    let variant = KeyValue::single(Value::Decimal(Decimal::parse("1.5").expect("decimal")));
    let m_variant = memory.row(&leaf(variant.clone())).expect("mem variant leaf");
    let p_variant = pg.row(&leaf(variant.clone())).expect("pg variant leaf");
    assert_eq!(
        m_variant, p_variant,
        "scale-variant intermediate-hop resolution diverged: reading /mid/1.5/leaf/1 must reach \
         the row inserted at /mid/1.500 on both backends — memory={m_variant:?} pg={p_variant:?}"
    );
    assert!(p_variant.is_some(), "the `1.5` hop must resolve the `1.500` intermediate node");

    // Durable reopen: the tombstone-through resolution still holds off the tree.
    let reopened = pg_factory.reopen(instance.clone()).expect("reopen pg store");
    assert_edges_match(&memory, &reopened, "reopened");
}

#[test]
fn deep_and_degenerate_addresses_never_panic() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("deepnopanic");
    let instance = InstanceId::new("deep-no-panic");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");

    // A depth-8 address (well within a well-formed tree, far below the corruption
    // tripwire) inserted from scratch — every ancestor auto-created as a tombstone.
    let deep = addr(&[
        ("l0", KeyValue::single(Value::Int(Integer::from(0)))),
        ("l1", KeyValue::single(Value::Int(Integer::from(1)))),
        ("l2", KeyValue::single(Value::Int(Integer::from(2)))),
        ("l3", KeyValue::single(Value::Int(Integer::from(3)))),
        ("l4", KeyValue::single(Value::Int(Integer::from(4)))),
        ("l5", KeyValue::single(Value::Int(Integer::from(5)))),
        ("l6", KeyValue::single(Value::Int(Integer::from(6)))),
        ("l7", KeyValue::single(Value::Int(Integer::from(7)))),
    ]);
    for store_is_pg in [false, true] {
        let outcome = if store_is_pg {
            let mut txn = pg.begin();
            txn.insert(deep.clone(), text("deep-8")).expect("pg deep insert");
            txn.commit()
        } else {
            let mut txn = memory.begin();
            txn.insert(deep.clone(), text("deep-8")).expect("mem deep insert");
            txn.commit()
        };
        outcome.expect("deep commit must not error");
    }

    // The deep row reads back identically; a never-existed sibling deep address is
    // cleanly `None`; a scan of a nonexistent collection is cleanly empty. None of
    // these panic or hang.
    assert_eq!(
        memory.row(&deep).expect("mem deep row"),
        pg.row(&deep).expect("pg deep row"),
        "deep depth-8 row diverged"
    );
    let ghost = addr(&[
        ("l0", KeyValue::single(Value::Int(Integer::from(0)))),
        ("l1", KeyValue::single(Value::Int(Integer::from(99)))),
        ("l2", KeyValue::single(Value::Int(Integer::from(2)))),
    ]);
    assert_eq!(memory.row(&ghost).expect("mem ghost"), pg.row(&ghost).expect("pg ghost"));
    assert!(pg.row(&ghost).expect("pg ghost").is_none(), "a never-existed deep read is None");

    let nowhere = CollectionPath::top(NameSegment::new("does-not-exist"));
    assert!(
        memory.scan(&nowhere).expect("mem empty scan").is_empty()
            && pg.scan(&nowhere).expect("pg empty scan").is_empty(),
        "a scan of an unpopulated collection is cleanly empty on both backends"
    );
    // `_guard` drops the throwaway schema on scope exit (and on a panic).
}
