//! Self-reconciling-schema correctness gates for the PostgreSQL backend.
//!
//! AGENTS.md makes the backend *self-reconciling*: every open brings the physical
//! schema into exact correspondence with the current model — it creates what is
//! missing and DROPs every orphan a superseded model or an older backend layout
//! left behind — so migrations never pollute the database. These gates lock that
//! in against the PostgreSQL system catalog, which is the **external oracle**: what
//! objects physically exist is read from `pg_catalog`/`information_schema`, never
//! from the crate's own bookkeeping, and compared to an independently-known desired
//! set (the seven fixed tables and the declared secondary indexes).
//!
//! The three orphan classes the reconciler eliminates each get a gate:
//!
//! - orphan **indexes** — a stray secondary index is dropped, the declared ones kept;
//! - orphan **tables** — a stray (empty) base table is dropped, the six fixed ones
//!   (incl. the §21-retained `commit_log`/`history_points`/`blobs`) kept;
//! - orphan **rows** — removing a collection (expressed as row deletes, the only way
//!   a removal reaches the store) leaves no residue in `nodes` while history is kept.
//!
//! Plus: reconciliation is idempotent (a second run neither creates nor drops), and
//! a fresh `create` materializes exactly the declared objects and nothing more.
//!
//! If no PostgreSQL is reachable, [`support::acquire`] fails loudly — the gates
//! never silently pass.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::collections::BTreeSet;

use liasse_ident::{HistoryPoint, InstanceId, LineageId, NameSegment, PointId};
use liasse_pg::Schema;
use liasse_store::{
    AddressStep, CollectionPath, CommitSeq, InstanceStore, KeyValue, RowAddress, StoreFactory,
    Transition,
};
use liasse_value::{Integer, Text, Value};
use postgres::Client;

use support::SchemaGuard;

/// The six fixed tables every instance schema owns — the externally-known desired
/// table set the live catalog is diffed against. `commit_log`/`history_points`/
/// `blobs` are the §21-retained stores that must never be treated as orphans;
/// `nodes` is the node-adjacency tree, the sole durable row representation.
const FIXED_TABLES: [&str; 6] =
    ["schema_version", "instance_meta", "nodes", "commit_log", "history_points", "blobs"];

/// The secondary indexes the current model declares — the externally-known desired
/// index set. Intrinsic primary-key indexes and `UNIQUE` table constraints are not
/// in this set (they drop with their tables and are never reconciled); a bare
/// `CREATE UNIQUE INDEX` like `node_key_lookup` is a managed secondary index and is.
const DECLARED_INDEXES: [&str; 1] = ["node_key_lookup"];

/// The live base tables in `schema`, read straight from `information_schema` — the
/// oracle for the desired-table diff.
fn live_tables(client: &mut Client, schema: &Schema) -> BTreeSet<String> {
    client
        .query(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = $1 AND table_type = 'BASE TABLE'",
            &[&schema.name()],
        )
        .expect("query live base tables")
        .iter()
        .map(|row| row.get::<_, String>(0))
        .collect()
}

/// The live *secondary* indexes in `schema`: those NOT backing a primary-key or
/// unique constraint. This is the reconciler's own reconcilable set, read from the
/// catalog — the oracle for the declared-index diff, and it excludes the intrinsic
/// PK indexes so their presence never masks an orphan.
fn live_secondary_indexes(client: &mut Client, schema: &Schema) -> BTreeSet<String> {
    client
        .query(
            "SELECT c.relname FROM pg_class c \
             JOIN pg_index x ON x.indexrelid = c.oid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind = 'i' \
               AND NOT EXISTS (SELECT 1 FROM pg_constraint con WHERE con.conindid = c.oid)",
            &[&schema.name()],
        )
        .expect("query live secondary indexes")
        .iter()
        .map(|row| row.get::<_, String>(0))
        .collect()
}

fn expected_tables() -> BTreeSet<String> {
    FIXED_TABLES.iter().map(|name| (*name).to_owned()).collect()
}

fn expected_indexes() -> BTreeSet<String> {
    DECLARED_INDEXES.iter().map(|name| (*name).to_owned()).collect()
}

/// A fresh `create` materializes exactly the declared objects — the six fixed
/// tables and every declared secondary index — and nothing extra.
#[test]
fn fresh_create_materializes_exactly_declared_objects() {
    let handle = support::acquire();
    let mut factory = handle.factory("freshcreate");
    let instance = InstanceId::new("fresh-create");
    let _guard = SchemaGuard::new(&factory, instance.clone());
    let schema = factory.schema_for(&instance);

    drop(factory.create(instance).expect("create provisions a fresh schema"));

    let mut client = factory.connect().expect("connect a raw client");
    assert_eq!(
        live_tables(&mut client, &schema),
        expected_tables(),
        "a fresh schema must hold exactly the six fixed tables, nothing more"
    );
    assert_eq!(
        live_secondary_indexes(&mut client, &schema),
        expected_indexes(),
        "a fresh schema must hold exactly the declared secondary indexes, nothing more"
    );
}

/// Reconciliation is idempotent: a second open neither creates nor drops anything,
/// leaving the live object set stable and equal to the declared desired set.
#[test]
fn reconcile_is_idempotent() {
    let handle = support::acquire();
    let mut factory = handle.factory("idempotent");
    let instance = InstanceId::new("idempotent");
    let _guard = SchemaGuard::new(&factory, instance.clone());
    let schema = factory.schema_for(&instance);

    drop(factory.create(instance.clone()).expect("first reconcile via create"));
    let mut client = factory.connect().expect("connect");
    let first_tables = live_tables(&mut client, &schema);
    let first_indexes = live_secondary_indexes(&mut client, &schema);

    // A second open reconciles again; nothing should move.
    drop(factory.reopen(instance).expect("second reconcile via reopen"));
    let second_tables = live_tables(&mut client, &schema);
    let second_indexes = live_secondary_indexes(&mut client, &schema);

    assert_eq!(first_tables, second_tables, "table set drifted across a second reconcile");
    assert_eq!(first_indexes, second_indexes, "index set drifted across a second reconcile");
    assert_eq!(second_tables, expected_tables(), "the stable table set must be the declared one");
    assert_eq!(second_indexes, expected_indexes(), "the stable index set must be the declared one");
}

/// A secondary index the model no longer declares is an orphan the reconciler must
/// drop, while the declared indexes survive.
#[test]
fn orphan_index_is_dropped() {
    let handle = support::acquire();
    let mut factory = handle.factory("orphanidx");
    let instance = InstanceId::new("orphan-index");
    let _guard = SchemaGuard::new(&factory, instance.clone());
    let schema = factory.schema_for(&instance);

    drop(factory.create(instance.clone()).expect("create"));

    // Inject a stray secondary index the declared set never contains.
    let mut client = factory.connect().expect("connect");
    client
        .batch_execute(&format!(
            "CREATE INDEX stray_orphan_idx ON {}.nodes (incarnation);",
            schema.quoted()
        ))
        .expect("create a stray index");
    assert!(
        live_secondary_indexes(&mut client, &schema).contains("stray_orphan_idx"),
        "the stray index must exist before reconciliation, or the gate proves nothing"
    );

    // Opening reconciles: the orphan must go, the declared index must stay.
    drop(factory.reopen(instance).expect("reopen reconciles"));

    let live = live_secondary_indexes(&mut client, &schema);
    assert!(!live.contains("stray_orphan_idx"), "orphan index survived reconciliation: {live:?}");
    assert_eq!(live, expected_indexes(), "reconciliation must leave exactly the declared indexes");
}

/// An *empty* base table not in the fixed set is an orphan (a leftover from a prior
/// backend layout) the reconciler must drop, while the six fixed tables — including
/// the §21-retained history and blob stores — survive. (A *populated* orphan is
/// refused rather than dropped; `populated_orphan_table_is_refused` covers that.)
#[test]
fn orphan_table_is_dropped() {
    let handle = support::acquire();
    let mut factory = handle.factory("orphantbl");
    let instance = InstanceId::new("orphan-table");
    let _guard = SchemaGuard::new(&factory, instance.clone());
    let schema = factory.schema_for(&instance);

    drop(factory.create(instance.clone()).expect("create"));

    // Inject a stray EMPTY table the fixed set never contains.
    let mut client = factory.connect().expect("connect");
    client
        .batch_execute(&format!(
            "CREATE TABLE {}.stray_orphan_table (x INT PRIMARY KEY);",
            schema.quoted()
        ))
        .expect("create a stray table");
    assert!(
        live_tables(&mut client, &schema).contains("stray_orphan_table"),
        "the stray table must exist before reconciliation, or the gate proves nothing"
    );

    // Opening reconciles: the empty orphan must go, the six fixed tables must stay.
    drop(factory.reopen(instance).expect("reopen reconciles"));

    let live = live_tables(&mut client, &schema);
    assert!(!live.contains("stray_orphan_table"), "orphan table survived reconciliation: {live:?}");
    assert_eq!(live, expected_tables(), "reconciliation must leave exactly the six fixed tables");
    for retained in ["commit_log", "history_points", "blobs"] {
        assert!(live.contains(retained), "reconciliation dropped the §21-retained `{retained}`");
    }
}

/// A *populated* orphan table is REFUSED, not silently `CASCADE`-dropped: reopening
/// a legacy layout whose leftover table still holds data must fail loudly rather
/// than destroy it. The refusal rolls the reconcile back, so the orphan and its
/// rows survive for a manual export/drop.
#[test]
fn populated_orphan_table_is_refused() {
    let handle = support::acquire();
    let mut factory = handle.factory("poporphan");
    let instance = InstanceId::new("populated-orphan");
    let _guard = SchemaGuard::new(&factory, instance.clone());
    let schema = factory.schema_for(&instance);

    drop(factory.create(instance.clone()).expect("create"));

    // Inject a stray table AND put a row in it — a stand-in for a pre-node `rows`
    // table a legacy database still carries.
    let mut client = factory.connect().expect("connect");
    client
        .batch_execute(&format!(
            "CREATE TABLE {s}.legacy_rows (x INT PRIMARY KEY); \
             INSERT INTO {s}.legacy_rows (x) VALUES (1);",
            s = schema.quoted()
        ))
        .expect("create and populate a stray table");

    // Reopening reconciles: the populated orphan must be refused, not dropped.
    let refused = factory.reopen(instance);
    assert!(refused.is_err(), "reopening over a populated orphan table must be refused");

    // The refusal rolled back untouched: the orphan and its row are still there.
    let live = live_tables(&mut client, &schema);
    assert!(
        live.contains("legacy_rows"),
        "a refused reconcile must not have dropped the populated orphan: {live:?}"
    );
    let count: i64 = client
        .query_one(&format!("SELECT COUNT(*) FROM {}.legacy_rows", schema.quoted()), &[])
        .expect("count legacy rows")
        .get(0);
    assert_eq!(count, 1, "the orphan's data must survive a refused reconcile");
}

/// Removing a collection leaves NO LIVE rows in `nodes`, while the kept collection
/// and all history remain readable. A collection is a subtree of the node tree (its
/// direct rows share a `(parent_id, step_name)`), so the runtime expresses a §20
/// removal as a Delete per row — the only way a removal reaches the store. Each
/// Delete tombstones its node (`value` NULL); no live row of the removed collection
/// survives. The inert tombstones that linger — deleted leaf rows with no descendant
/// to keep addressable — are a future GC opportunity, correctness-neutral, so this
/// gate counts LIVE rows (`value IS NOT NULL`).
#[test]
fn removing_a_collection_leaves_no_orphan_rows() {
    let handle = support::acquire();
    let mut factory = handle.factory("orphanrows");
    let instance = InstanceId::new("orphan-rows");
    let _guard = SchemaGuard::new(&factory, instance.clone());
    let schema = factory.schema_for(&instance);

    let keep = CollectionPath::top(NameSegment::new("keep"));
    let drop = CollectionPath::top(NameSegment::new("drop"));
    let addr = |collection: &str, key: i64| {
        RowAddress::root(AddressStep::new(
            NameSegment::new(collection),
            KeyValue::single(Value::Int(Integer::from(key))),
        ))
    };
    let point = HistoryPoint::new(LineageId::new("main"), PointId::new("before-migration"));

    {
        let mut store = factory.create(instance.clone()).expect("create");

        // Load a two-collection model.
        let mut txn = store.begin();
        txn.insert(addr("keep", 1), Value::Text(Text::new("k1"))).expect("insert keep 1");
        txn.insert(addr("keep", 2), Value::Text(Text::new("k2"))).expect("insert keep 2");
        txn.insert(addr("drop", 1), Value::Text(Text::new("d1"))).expect("insert drop 1");
        txn.insert(addr("drop", 2), Value::Text(Text::new("d2"))).expect("insert drop 2");
        txn.insert(addr("drop", 3), Value::Text(Text::new("d3"))).expect("insert drop 3");
        txn.commit().expect("commit inserts");

        // Mark a history point before migrating.
        let head = store.head().unwrap();
        store.record_point(head, point.clone()).expect("record point");

        // Migrate to a model without the `drop` collection: remove its rows.
        let doomed: Vec<RowAddress> =
            store.scan(&drop).expect("scan drop").into_iter().map(|(address, _)| address).collect();
        assert_eq!(doomed.len(), 3, "the drop collection held three rows before migration");
        let mut txn = store.begin();
        for address in &doomed {
            txn.delete(address).expect("delete a drop row");
        }
        txn.commit().expect("commit migration");
    }

    // Node/catalog oracle: not one LIVE `drop` node survives under the root sentinel;
    // both `keep` nodes do. `keep`/`drop` are top-level collections, so their direct
    // rows are nodes with `parent_id = 0` (the sentinel) and the matching `step_name`;
    // `value IS NOT NULL` counts live rows, ignoring the inert tombstones a Delete
    // leaves behind (a GC opportunity, correctness-neutral).
    let mut client = factory.connect().expect("connect");
    let s = schema.quoted();
    let count_collection = |client: &mut Client, name: &str| -> i64 {
        client
            .query_one(
                &format!(
                    "SELECT COUNT(*) FROM {s}.nodes \
                     WHERE parent_id = 0 AND step_name = $1 AND value IS NOT NULL"
                ),
                &[&name],
            )
            .expect("count nodes by collection")
            .get(0)
    };
    assert_eq!(count_collection(&mut client, "drop"), 0, "removed collection left live rows");
    assert_eq!(count_collection(&mut client, "keep"), 2, "kept collection nodes must remain");

    // History is retained (§21): both commits and the recorded point survive.
    let commits: i64 = client
        .query_one(&format!("SELECT COUNT(*) FROM {s}.commit_log"), &[])
        .expect("count commit_log")
        .get(0);
    assert_eq!(commits, 2, "both commits must remain in the retained log");
    let points: i64 = client
        .query_one(&format!("SELECT COUNT(*) FROM {s}.history_points"), &[])
        .expect("count history_points")
        .get(0);
    assert_eq!(points, 1, "the recorded history point must remain");

    // End-to-end: a projection rebuilt from the durable tables on reopen agrees —
    // which also proves the retained log and points are readable (the load reads them).
    let reopened = factory.reopen(instance).expect("reopen");
    assert!(reopened.scan(&drop).expect("scan drop").is_empty(), "removed collection empty after reopen");
    assert_eq!(reopened.scan(&keep).expect("scan keep").len(), 2, "kept collection survives reopen");
    assert_eq!(
        reopened.log_from(CommitSeq::GENESIS).expect("read log").len(),
        2,
        "the retained log is readable after reopen"
    );
    assert!(reopened.point_position(&point).unwrap().is_some(), "history point resolvable after reopen");
}
