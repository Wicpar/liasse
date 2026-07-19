//! Index-coverage correctness gates for the PostgreSQL backend.
//!
//! CLAUDE.md makes backend performance a *correctness gate*: every SQL query
//! pattern the schema must serve has to be backed by an appropriate index, never
//! degrading to a sequential scan on a populated table. These gates lock that in
//! **deterministically** — they read the query planner, not a clock, so the
//! "never write performance tests" rule does not apply (this is a plan-shape
//! invariant, not a timing measurement).
//!
//! # What each gate does
//!
//! Each gate provisions the *real* schema DDL ([`Schema::create_ddl`], indexes and
//! all), populates the relevant table well past the point where PostgreSQL would
//! prefer a sequential scan, `ANALYZE`s it so the planner has honest statistics,
//! then runs `EXPLAIN (FORMAT JSON)` on the pattern and walks the plan tree
//! asserting:
//!
//! - no `Seq Scan` node anywhere in the plan, and
//! - at least one `Index Scan` / `Index Only Scan` / `Bitmap Index Scan`, and
//! - for the ordered patterns, no `Sort` node (the index supplies the order).
//!
//! A missing or ineffective index makes the gate **fail**.
//!
//! # Relationship to the runtime read path
//!
//! Under the pure-PG re-architecture (`DESIGN-pure-pg.md`), `PgStore` answers the
//! contract's `&self` reads with one indexed SQL statement each on a pooled
//! connection: the leaf reads (Phase 1) and the `row`/`scan` node reads (Phase 2,
//! §4.1/§4.2, [`liasse_pg`]'s `read` path). These gates are the READ gate for that
//! path AND a property of the durable *schema*: every query pattern the backend runs
//! against PostgreSQL — the write path's keyed node mutations and the chained-InitPlan
//! reads — has an index to ride, so it can never silently become a full scan. The node
//! point lookup covers `(parent_id, step_name, key_enc)`; the chained InitPlan (gates
//! 7/8) resolves a nested address level by level; the ordered node scan covers a
//! collection read in Annex B key order (`key_enc` is `BYTEA`, compared by unsigned
//! `memcmp`, so the index supplies the order with no `COLLATE` and no `Sort`).
//!
//! The `instance_meta` / `schema_version` head-and-version reads (`WHERE id = 1`)
//! are deliberately absent: those tables are constrained to a single row
//! (`CHECK (id = 1)`), so a sequential scan of one row is optimal and no index can
//! (or should) change the plan. [`meta_tables_are_single_row`] pins that rationale.
//!
//! If no PostgreSQL is reachable, [`support::acquire`] fails loudly with how to set
//! `LIASSE_PG_TEST_DSN` — the gates never silently pass.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use liasse_ident::InstanceId;
use liasse_pg::{PgStoreFactory, Schema};
use postgres::Client;
use postgres::types::ToSql;
use serde_json::Value as J;

use support::SchemaGuard;

/// Sibling "noise" nodes to insert before `ANALYZE` — comfortably past any
/// small-table seq-scan preference, so a selective indexed read is unambiguously the
/// cheaper plan.
const POP: i64 = 20_000;

/// Direct rows in the one target collection whose point lookup and ordered scan the
/// gates plan. Kept small (a selective fraction of `POP`), so the node lookup index
/// is decisively preferred over a full scan of the table.
const COLLECTION: i64 = 64;

/// Provision the real schema DDL over a raw connection, returning the client and
/// the schema so a gate can populate its tables and `EXPLAIN` its patterns. A
/// [`SchemaGuard`] on the returned instance drops everything at end of test.
fn provision(factory: &PgStoreFactory, instance: &InstanceId) -> (Client, Schema) {
    let schema = factory.schema_for(instance);
    let mut client = factory.connect().expect("connect a raw client");
    client.batch_execute(&schema.drop_ddl()).expect("clean slate");
    client.batch_execute(&schema.create_ddl()).expect("apply real DDL");
    (client, schema)
}

/// The 8-byte big-endian `key_enc` for integer `n` — byte-identical to Postgres
/// `int8send(n)` (which the SQL populator uses), memcmp-ordered for the non-negative
/// keys these gates seed.
fn key_enc(n: i64) -> Vec<u8> {
    n.to_be_bytes().to_vec()
}

/// Run `EXPLAIN (FORMAT JSON) <sql>` with `params` bound, returning the top plan
/// node. Binding real parameter values makes the planner produce the same custom
/// plan the backend's own extended-protocol execution would.
fn explain(client: &mut Client, sql: &str, params: &[&(dyn ToSql + Sync)]) -> J {
    let row = client
        .query_one(&format!("EXPLAIN (FORMAT JSON) {sql}"), params)
        .expect("EXPLAIN runs");
    let doc: J = row.get(0);
    doc.get(0)
        .and_then(|entry| entry.get("Plan"))
        .cloned()
        .unwrap_or_else(|| panic!("EXPLAIN JSON had no root Plan: {doc}"))
}

/// Collect every `Node Type` in the plan tree, depth first.
fn node_types(plan: &J) -> Vec<String> {
    let mut out = Vec::new();
    walk(plan, &mut out);
    out
}

fn walk(plan: &J, out: &mut Vec<String>) {
    if let Some(node) = plan.as_str() {
        out.push(node.to_owned());
        return;
    }
    if let Some(kind) = plan.get("Node Type").and_then(J::as_str) {
        out.push(kind.to_owned());
    }
    if let Some(children) = plan.get("Plans").and_then(J::as_array) {
        for child in children {
            walk(child, out);
        }
    }
}

/// Assert a plan is served by an index and never scans the whole table.
fn assert_index_only(plan: &J, label: &str) {
    let types = node_types(plan);
    assert!(
        !types.iter().any(|t| t == "Seq Scan"),
        "{label}: plan falls back to a Seq Scan (missing/ineffective index): {types:?}"
    );
    assert!(
        types.iter().any(|t| t == "Index Scan" || t == "Index Only Scan" || t == "Bitmap Index Scan"),
        "{label}: plan uses no index scan: {types:?}"
    );
}

/// As [`assert_index_only`], and additionally that the index supplies the order —
/// no explicit `Sort` node stands between the scan and the result.
fn assert_index_ordered(plan: &J, label: &str) {
    assert_index_only(plan, label);
    let types = node_types(plan);
    assert!(
        !types.iter().any(|t| t == "Sort" || t == "Incremental Sort"),
        "{label}: plan sorts rather than reading the index in order: {types:?}"
    );
}

/// Populate every table this gate suite queries, then `ANALYZE`; return the
/// surrogate id of the container node whose `items` collection the node gates plan.
///
/// The node tree is seeded so the target collection is a *selective* slice: one
/// container parent holds a small `items` collection, while a large body of sibling
/// "noise" nodes bloats the table — so `WHERE parent_id = P AND step_name = 'items'`
/// selects a tiny fraction and the planner rides `node_key_lookup` rather than
/// scanning the whole table.
fn populate(client: &mut Client, schema: &Schema) -> i64 {
    let s = schema.quoted();
    // Root sentinel (id = 0) every depth-1 node hangs under.
    client
        .execute(
            &format!(
                "INSERT INTO {s}.nodes \
                 (id, parent_id, step_name, key_enc, key_wire, incarnation, value) \
                 OVERRIDING SYSTEM VALUE \
                 VALUES (0, 0, '', '\\x'::bytea, '{{}}'::jsonb, '', '{{}}'::jsonb) \
                 ON CONFLICT (id) DO NOTHING"
            ),
            &[],
        )
        .expect("seed root sentinel");
    // One container parent under the sentinel; capture its surrogate id.
    let parent: i64 = client
        .query_one(
            &format!(
                "INSERT INTO {s}.nodes \
                 (parent_id, step_name, key_enc, key_wire, incarnation, value) \
                 VALUES (0, 'orgs', $1, '{{}}'::jsonb, '', '{{}}'::jsonb) RETURNING id"
            ),
            &[&key_enc(1)],
        )
        .expect("insert container node")
        .get("id");
    // A depth-3 chain `/orgs/1/teams/1/members/*` under the container, so the chained
    // InitPlan resolution (gates 7/8) has real intermediate hops to plan. `teams/1`
    // is captured so its `members` collection can be populated as a selective slice.
    let team: i64 = client
        .query_one(
            &format!(
                "INSERT INTO {s}.nodes \
                 (parent_id, step_name, key_enc, key_wire, incarnation, value) \
                 VALUES ({parent}, 'teams', $1, '{{}}'::jsonb, 'row-t', '{{}}'::jsonb) RETURNING id"
            ),
            &[&key_enc(1)],
        )
        .expect("insert depth-2 team node")
        .get("id");
    client
        .batch_execute(&format!(
            "INSERT INTO {s}.nodes \
               (parent_id, step_name, key_enc, key_wire, incarnation, value) \
               SELECT {team}, 'members', int8send(g::int8), '{{}}'::jsonb, 'row-m' || g, \
                      jsonb_build_object('s', g::text) \
               FROM generate_series(0, {collection_last}) AS g;\n\
             INSERT INTO {s}.nodes \
               (parent_id, step_name, key_enc, key_wire, incarnation, value) \
               SELECT {parent}, 'items', int8send(g::int8), '{{}}'::jsonb, 'row-' || g, \
                      jsonb_build_object('s', g::text) \
               FROM generate_series(0, {collection_last}) AS g;\n\
             INSERT INTO {s}.nodes \
               (parent_id, step_name, key_enc, key_wire, incarnation, value) \
               SELECT 0, 'noise', int8send(g::int8), '{{}}'::jsonb, 'row-' || g, \
                      jsonb_build_object('s', g::text) \
               FROM generate_series(1, {pop}) AS g;\n\
             INSERT INTO {s}.commit_log (seq, transaction_id, ops) \
               SELECT g, NULL, '[]'::jsonb FROM generate_series(1, {pop}) AS g;\n\
             INSERT INTO {s}.history_points (lineage, point, seq) \
               SELECT 'lin-' || (g % 50), 'pt-' || g, g FROM generate_series(1, {pop}) AS g;\n\
             INSERT INTO {s}.blobs (digest, bytes) \
               SELECT lpad(g::text, 128, '0'), decode(md5(g::text), 'hex') \
               FROM generate_series(1, {pop}) AS g;\n\
             ANALYZE {s}.nodes; ANALYZE {s}.commit_log; \
             ANALYZE {s}.history_points; ANALYZE {s}.blobs;",
            collection_last = COLLECTION - 1,
            pop = POP,
        ))
        .expect("populate and analyze");
    parent
}

/// Every enumerated backend query pattern is index-served on a populated table.
/// One test so a single provisioning + populate is amortized across all gates,
/// while each pattern still fails with its own labelled assertion.
#[test]
fn every_query_pattern_is_index_served() {
    let handle = support::acquire();
    let factory = handle.factory("explain");
    let instance = InstanceId::new("index-gate");
    let _guard = SchemaGuard::new(&factory, instance.clone());

    let (mut client, schema) = provision(&factory, &instance);
    let parent = populate(&mut client, &schema);
    let s = schema.quoted();
    let items = "items";

    // (1) Node point lookup by (parent_id, step_name, key_enc) — the write path's
    // row resolution, served by the `node_key_lookup` unique index. `value IS NOT
    // NULL` excludes tombstones (deleted ancestors kept only so descendants stay
    // addressable); a live-row read must never surface one.
    let key = key_enc(COLLECTION / 2);
    let plan = explain(
        &mut client,
        &format!(
            "SELECT id, incarnation, value FROM {s}.nodes \
             WHERE parent_id = $1 AND step_name = $2 AND key_enc = $3 AND value IS NOT NULL"
        ),
        &[&parent, &items, &key],
    );
    assert_index_only(&plan, "node point lookup by (parent_id, step_name, key_enc)");

    // (2) Collection scan in Annex B key order — all direct LIVE rows of one
    // collection in `key_enc` order, served by the same `node_key_lookup` index.
    // `key_enc` is BYTEA (unsigned `memcmp`), so the index supplies the order with no
    // `COLLATE` and the plan carries no `Sort`; `value IS NOT NULL` (an in-scan
    // filter) excludes tombstones without disturbing the index order.
    let plan = explain(
        &mut client,
        &format!(
            "SELECT id, key_enc, incarnation, value FROM {s}.nodes \
             WHERE parent_id = $1 AND step_name = $2 AND value IS NOT NULL ORDER BY key_enc"
        ),
        &[&parent, &items],
    );
    assert_index_ordered(&plan, "node collection scan in key order");

    // The chained scalar-subquery (InitPlan) that resolves the parent of a depth-3
    // address `/orgs/1/teams/1/…` from the root sentinel, hopping
    // `(parent_id, step_name, key_enc)` per level with NO `value IS NOT NULL` filter
    // on the intermediate hops (§4.1: a resolve walks through a tombstoned ancestor).
    // This is the exact form `read.rs` generates.
    let teams = "teams";
    let members = "members";
    let chain = format!(
        "(SELECT id FROM {s}.nodes WHERE parent_id = \
           (SELECT id FROM {s}.nodes WHERE parent_id = 0 AND step_name = $1 AND key_enc = $2) \
           AND step_name = $3 AND key_enc = $4)"
    );

    // (7) depth-3 `row` chained-InitPlan point lookup — the outermost level adds
    // `value IS NOT NULL`; every hop is an `Index Scan using node_key_lookup`
    // (InitPlan), so the whole walk is index-served with no Seq Scan at any depth.
    let member = key_enc(COLLECTION / 2);
    let plan = explain(
        &mut client,
        &format!(
            "SELECT incarnation, value FROM {s}.nodes \
             WHERE parent_id = {chain} AND step_name = $5 AND key_enc = $6 AND value IS NOT NULL"
        ),
        &[&"orgs", &key_enc(1), &teams, &key_enc(1), &members, &member],
    );
    assert_index_only(&plan, "depth-3 row chained-InitPlan point lookup");

    // (8) depth-3 `scan` in the §4.2 SCALAR-SUBQUERY form — the same chain resolves
    // the parent, then the ordered child range over the final level rides
    // `node_key_lookup` in `key_enc` (BYTEA memcmp = Annex B) order: NO `Sort` node,
    // no Seq Scan. This pins the scalar-subquery formulation against a flat-JOIN
    // regression (a join makes the planner insert a `Sort`).
    let plan = explain(
        &mut client,
        &format!(
            "SELECT key_wire, incarnation, value FROM {s}.nodes \
             WHERE parent_id = {chain} AND step_name = $5 AND value IS NOT NULL ORDER BY key_enc"
        ),
        &[&"orgs", &key_enc(1), &teams, &key_enc(1), &members],
    );
    assert_index_ordered(&plan, "depth-3 scan §4.2 chained-InitPlan, index-ordered no Sort");

    // (3) Commit-log read from a seq (log_from / replay) — WHERE seq >= $1 ORDER BY
    // seq, served in order by the `commit_log` primary key.
    let from: i64 = POP - 10;
    let plan = explain(
        &mut client,
        &format!("SELECT seq, transaction_id, ops FROM {s}.commit_log WHERE seq >= $1 ORDER BY seq"),
        &[&from],
    );
    assert_index_ordered(&plan, "commit_log read from a seq");

    // (4) Snapshot-at-frontier fold — WHERE seq <= $frontier ORDER BY seq, again
    // ordered by the `commit_log` primary key.
    let frontier: i64 = 20;
    let plan = explain(
        &mut client,
        &format!("SELECT seq, ops FROM {s}.commit_log WHERE seq <= $1 ORDER BY seq"),
        &[&frontier],
    );
    assert_index_ordered(&plan, "snapshot-at-frontier fold");

    // (5) Blob lookup by digest — served by the `blobs` primary key.
    let digest = format!("{:0>128}", POP / 2);
    let plan =
        explain(&mut client, &format!("SELECT bytes FROM {s}.blobs WHERE digest = $1"), &[&digest]);
    assert_index_only(&plan, "blob lookup by digest");

    // (6) History-point lookup — WHERE lineage = $1 AND point = $2, served by the
    // `history_points` composite primary key.
    let lineage = format!("lin-{}", (POP / 2) % 50);
    let point = format!("pt-{}", POP / 2);
    let plan = explain(
        &mut client,
        &format!("SELECT seq FROM {s}.history_points WHERE lineage = $1 AND point = $2"),
        &[&lineage, &point],
    );
    assert_index_only(&plan, "history-point lookup");

    // (10) `has_blob` existence probe — SELECT EXISTS(SELECT 1 FROM blobs WHERE
    // digest = $1), the leaf `has_blob` read (§4.4). The `EXISTS` collapses the
    // match to a single boolean, and the inner probe rides the `blobs` primary key:
    // an index-only lookup, never a Seq Scan of the populated table.
    let digest = format!("{:0>128}", POP / 3);
    let plan = explain(
        &mut client,
        &format!("SELECT EXISTS(SELECT 1 FROM {s}.blobs WHERE digest = $1) AS present"),
        &[&digest],
    );
    assert_index_only(&plan, "has_blob EXISTS probe by digest");
}

/// The set of derived indexes is enumerable and each one is actually
/// materialized by the DDL — the property a later reconciliation round relies on
/// to diff live indexes against the model and drop the orphans.
#[test]
fn derived_indexes_are_all_created() {
    let handle = support::acquire();
    let factory = handle.factory("idxset");
    let instance = InstanceId::new("index-set");
    let _guard = SchemaGuard::new(&factory, instance.clone());

    let (mut client, schema) = provision(&factory, &instance);
    let derived = schema.indexes();
    assert!(!derived.is_empty(), "the model derives at least one secondary index");

    for spec in &derived {
        let found = client
            .query_one(
                "SELECT COUNT(*) FROM pg_indexes WHERE schemaname = $1 AND indexname = $2",
                &[&schema.name(), &spec.name()],
            )
            .expect("query pg_indexes");
        let count: i64 = found.get(0);
        assert_eq!(count, 1, "derived index `{}` was not materialized", spec.name());
    }
}

/// The head/version tables are single-row by construction, which is why they are
/// exempt from the index gate: a sequential scan of one row is optimal, and the
/// `CHECK (id = 1)` keeps them that way.
#[test]
fn meta_tables_are_single_row() {
    let handle = support::acquire();
    let mut factory = handle.factory("meta1");
    let instance = InstanceId::new("meta-single");
    let _guard = SchemaGuard::new(&factory, instance.clone());

    use liasse_store::StoreFactory;
    let _store = factory.create(instance.clone()).expect("create provisions meta rows");
    let schema = factory.schema_for(&instance);
    let s = schema.quoted();
    let mut client = factory.connect().expect("connect");

    for table in ["instance_meta", "schema_version"] {
        let row = client
            .query_one(&format!("SELECT COUNT(*) FROM {s}.{table}"), &[])
            .expect("count rows");
        let count: i64 = row.get(0);
        assert_eq!(count, 1, "{table} must hold exactly one row (id = 1)");
    }
}
