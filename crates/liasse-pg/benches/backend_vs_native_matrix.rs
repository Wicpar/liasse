//! Comparative benchmark MATRIX: the `liasse-pg` backend path vs. the equivalent
//! hand-written native-PostgreSQL SQL, swept across dataset SIZES and QUERY CLASSES.
//!
//! # Why a matrix, and what the numbers mean
//!
//! `PgStore` answers every contract read (`row`, `scan`) from an in-memory
//! PROJECTION rebuilt from the durable `nodes` tree on open; PostgreSQL is the
//! durable *write* path. The store is also **semantics-free** — it has no `$view`,
//! filter, join, or aggregate of its own. So a runtime built on the contract serves
//! a complex query by `scan`ning the collection(s) from the projection and doing the
//! filter / join / aggregate / view-projection **in Rust** over the returned rows.
//! That is exactly the "backend path" measured here for the complex classes.
//!
//! This makes the honest methodology explicit, per class:
//!
//! - **Read classes (1-7)** compare a PROJECTION-served backend path (RAM: a
//!   `BTreeMap` scan plus in-Rust computation) against the native SQL a hand-written
//!   query would issue (a round trip to PostgreSQL, index-served where an index
//!   applies). The ratio therefore shows the projection's SAVING or its OVERHEAD —
//!   it is NOT a same-substrate race. Each row is labelled `projection vs SQL`.
//! - **Write class (8)** is the one genuinely like-for-like axis: the backend's
//!   admission transaction (head lock, log append, node insert, head bump) versus
//!   the identical four statements issued by hand against an isolated twin schema
//!   populated to the SAME size, so both insert into an equally-indexed table. Row
//!   labelled `like-for-like SQL`.
//!
//! Reads are timed only after the projection is warm. Every measured value is fed
//! through `black_box`. Alongside the `criterion` groups, a self-timed median pass
//! (real wall-clock — a normal bench binary, so `Instant` is fine) computes the
//! per-op medians and the backend÷raw ratio printed in the summary table.
//!
//! # Fixtures
//!
//! Each size's fixture is bulk-built once with `INSERT … SELECT generate_series`
//! (committing 100k rows one-by-one through the contract would dominate the run),
//! then the store is REOPENED so its projection loads the whole set from `nodes`.
//! The wire forms written by the bulk SQL are byte-for-byte what the backend's own
//! codecs produce (`key_wire` = `[{"i":"<k>"}]`; a struct `value` =
//! `{"st":[["<field>",<tagged>],…]}` with fields in `Struct`'s sorted-name order),
//! so the projection decodes them exactly as a live insert would.
//!
//! Requires a database, resolved (or bootstrapped) by the shared test support module
//! exactly as the integration tests do; `cargo bench --no-run` only compiles it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

#[path = "../tests/support/mod.rs"]
mod support;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use liasse_ident::{InstanceId, NameSegment};
use liasse_pg::{PgStore, PgStoreFactory};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, RowAddress, StoreFactory, StoredRow,
    Transition,
};
use liasse_value::{Integer, Text, Value};
use postgres::Client;

use support::SchemaGuard;

/// Dataset sizes swept. SMALL exercises the small-table regime; LARGE (100k) is well
/// past any seq-scan preference so the native baselines must ride their indexes.
/// Extend to the 1M tier by appending `1_000_000` — the fixture is bulk-built, so it
/// scales; expect the projection load and the whole-collection scans to grow O(N).
const SIZES: &[i64] = &[1_000, 100_000];

/// The root sentinel `nodes.id`; every top-level row hangs directly under it, so a
/// top-level collection is `WHERE parent_id = 0 AND step_name = '<name>'`.
const SENTINEL: i64 = 0;

/// Distinct `orgs` the `items` reference — the small dimension the join/traversal
/// class resolves against. Each item's `org` field is `key % ORG_COUNT`, so every
/// item matches exactly one org.
const ORG_COUNT: i64 = 64;

/// Buckets an item's `bucket` field cycles through (`key % BUCKETS`); the filter and
/// view classes select one bucket, ≈ 1/BUCKETS of the collection.
const BUCKETS: i64 = 10;

/// The bucket the filter/view classes select.
const FILTER_BUCKET: i64 = 7;

/// Width of the moderate key-range / prefix scan (keys `[N/2, N/2+RANGE_SPAN)`).
const RANGE_SPAN: i64 = 128;

/// Self-timed median samples per op (odd, so the median is a real sample). Kept
/// modest because the heavy whole-collection scans cost tens of ms at LARGE.
const SELF_ITERS: usize = 31;

/// The order-preserving `key_enc` bytes for a non-negative integer key — the 8-byte
/// big-endian form, byte-identical to PostgreSQL `int8send(k)` (which the bulk
/// populator writes) and `memcmp`-ordered for the non-negative keys seeded here.
fn key_enc_be(key: i64) -> Vec<u8> {
    key.to_be_bytes().to_vec()
}

/// The address of item `key` in the top-level `items` collection.
fn item_address(key: i64) -> RowAddress {
    RowAddress::root(AddressStep::new(
        NameSegment::new("items"),
        KeyValue::single(Value::Int(Integer::from(key))),
    ))
}

/// The address of `key` in the top-level `writes` collection the write class commits
/// into — kept separate from the `items` fixture so a commit never collides with it.
fn write_address(key: i64) -> RowAddress {
    RowAddress::root(AddressStep::new(
        NameSegment::new("writes"),
        KeyValue::single(Value::Int(Integer::from(key))),
    ))
}

/// The integer at the final level of `address`, if its key is a single `int`.
fn addr_key_int(address: &RowAddress) -> Option<i64> {
    let step = address.steps().last()?;
    let mut components = step.key().components();
    match (components.next(), components.next()) {
        (Some(Value::Int(i)), None) => i.to_canonical_text().parse().ok(),
        _ => None,
    }
}

/// An `int` field of a struct-valued row, resolved by name (order-independent).
fn struct_int_field(row: &StoredRow, name: &str) -> Option<i64> {
    match row.value() {
        Value::Struct(fields) => match fields.get(name) {
            Some(Value::Int(i)) => i.to_canonical_text().parse().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// A `text` field of a struct-valued row, resolved by name.
fn struct_text_field(row: &StoredRow, name: &str) -> Option<String> {
    match row.value() {
        Value::Struct(fields) => match fields.get(name) {
            Some(Value::Text(t)) => Some(t.as_str().to_owned()),
            _ => None,
        },
        _ => None,
    }
}

/// Bulk-populate `schema`'s `nodes` with `orgs` and `items`, then `ANALYZE`. The
/// struct `value` lays fields out in `Struct`'s sorted-name order — the same order
/// the backend's own encoder emits — so native positional access (`->'st'->i->1`)
/// and the projection's name-keyed decode agree.
///
/// `orgs` fields (sorted): `name`(0), `region`(1). `items` fields (sorted):
/// `bucket`(0), `label`(1), `org`(2).
fn populate(client: &mut Client, schema: &str, items: i64) {
    let sql = format!(
        "INSERT INTO {schema}.nodes (parent_id, step_name, key_enc, key_wire, incarnation, value) \
           SELECT 0, 'orgs', int8send(g::int8), \
                  jsonb_build_array(jsonb_build_object('i', g::text)), \
                  'inc-org-' || g, \
                  jsonb_build_object('st', jsonb_build_array( \
                    jsonb_build_array('name',   jsonb_build_object('s', 'org-' || g)), \
                    jsonb_build_array('region', jsonb_build_object('i', (g % 5)::text)))) \
           FROM generate_series(0, {org_last}) AS g;\n\
         INSERT INTO {schema}.nodes (parent_id, step_name, key_enc, key_wire, incarnation, value) \
           SELECT 0, 'items', int8send(g::int8), \
                  jsonb_build_array(jsonb_build_object('i', g::text)), \
                  'inc-item-' || g, \
                  jsonb_build_object('st', jsonb_build_array( \
                    jsonb_build_array('bucket', jsonb_build_object('i', (g % {buckets})::text)), \
                    jsonb_build_array('label',  jsonb_build_object('s', 'row-' || lpad(g::text, 8, '0'))), \
                    jsonb_build_array('org',    jsonb_build_object('i', (g % {orgs})::text)))) \
           FROM generate_series(0, {item_last}) AS g;\n\
         ANALYZE {schema}.nodes;",
        org_last = ORG_COUNT - 1,
        item_last = items - 1,
        buckets = BUCKETS,
        orgs = ORG_COUNT,
    );
    client.batch_execute(&sql).expect("bulk-populate fixture");
}

/// The whole fixture for one size: a store whose projection is loaded with `items`
/// rows, a twin schema populated to the same size for the like-for-like commit
/// baseline, and a raw client for the native baselines.
struct Fixture {
    store: PgStore,
    items: CollectionPath,
    orgs: CollectionPath,
    store_schema: String,
    twin_schema: String,
    _store_guard: SchemaGuard,
    _twin_guard: SchemaGuard,
}

impl Fixture {
    fn build(factory: &mut PgStoreFactory, size: i64) -> (Self, Client) {
        let store_instance = InstanceId::new(format!("matrix-store-{size}"));
        let twin_instance = InstanceId::new(format!("matrix-twin-{size}"));
        let store_guard = SchemaGuard::new(factory, store_instance.clone());
        let twin_guard = SchemaGuard::new(factory, twin_instance.clone());

        // `create` provisions each schema (root sentinel + `instance_meta`); the
        // returned handle is dropped because the fixture is bulk-loaded next.
        drop(factory.create(store_instance.clone()).expect("create store schema"));
        drop(factory.create(twin_instance.clone()).expect("create twin schema"));

        let store_schema = factory.schema_for(&store_instance).quoted();
        let twin_schema = factory.schema_for(&twin_instance).quoted();

        let mut client = factory.connect().expect("raw client");
        populate(&mut client, &store_schema, size);
        // The twin is populated to the same size so its `nodes` index has the same
        // depth: the commit baseline then inserts into an equally-loaded table.
        populate(&mut client, &twin_schema, size);

        // Reopen so the projection loads the bulk-inserted rows from `nodes`.
        let store = factory.reopen(store_instance).expect("reopen with populated projection");

        (
            Self {
                store,
                items: CollectionPath::top(NameSegment::new("items")),
                orgs: CollectionPath::top(NameSegment::new("orgs")),
                store_schema,
                twin_schema,
                _store_guard: store_guard,
                _twin_guard: twin_guard,
            },
            client,
        )
    }
}

/// Which substrates a row compares — the honesty label the report carries.
#[derive(Clone, Copy)]
enum Basis {
    /// A projection-served read (RAM) vs. the native SQL a hand query would run.
    Projection,
    /// The backend's admission transaction vs. the identical hand-issued SQL.
    LikeForLike,
}

impl Basis {
    fn label(self) -> &'static str {
        match self {
            Self::Projection => "projection vs SQL",
            Self::LikeForLike => "like-for-like SQL",
        }
    }
}

/// One measured (size × class) cell.
struct Record {
    size: i64,
    class: &'static str,
    basis: Basis,
    backend_ns: u128,
    raw_ns: u128,
}

/// Collects every cell and renders the results table + ratios + verdicts.
struct Recorder {
    rows: Vec<Record>,
}

impl Recorder {
    fn new() -> Self {
        Self { rows: Vec::new() }
    }

    fn push(&mut self, size: i64, class: &'static str, basis: Basis, backend_ns: u128, raw_ns: u128) {
        self.rows.push(Record { size, class, basis, backend_ns, raw_ns });
    }

    /// The backend÷raw ratio, and a one-word verdict interpreting it against the
    /// class's basis (a projection read winning is a "projection-win"; the
    /// like-for-like commit is judged for parity/overhead only).
    fn verdict(record: &Record) -> (f64, String) {
        let ratio = record.backend_ns as f64 / record.raw_ns.max(1) as f64;
        let word = match record.basis {
            Basis::Projection if ratio < 0.9 => format!("projection-win {:.1}x", 1.0 / ratio),
            Basis::LikeForLike if ratio < 0.9 => format!("backend-faster {:.1}x", 1.0 / ratio),
            _ if ratio <= 1.15 => "near-parity".to_owned(),
            _ => format!("overhead-{ratio:.1}x"),
        };
        (ratio, word)
    }

    fn report(&self) {
        println!("\n===== BACKEND vs NATIVE-PG MATRIX (medians) =====");
        println!(
            "{:>8}  {:<26}  {:<18}  {:>12}  {:>12}  {:>8}  verdict",
            "size", "class", "basis", "backend", "raw SQL", "ratio"
        );
        for record in &self.rows {
            let (ratio, word) = Self::verdict(record);
            println!(
                "{:>8}  {:<26}  {:<18}  {:>12}  {:>12}  {:>8}  {}",
                record.size,
                record.class,
                record.basis.label(),
                fmt_ns(record.backend_ns),
                fmt_ns(record.raw_ns),
                fmt_ratio(ratio),
                word,
            );
        }
        println!("=================================================\n");
    }
}

/// A backend÷raw ratio with enough significant digits to read at both extremes: a
/// point lookup's ~0.004 and a range scan's ~300× both stay legible.
fn fmt_ratio(ratio: f64) -> String {
    if ratio < 0.1 {
        format!("{ratio:.4}")
    } else if ratio < 10.0 {
        format!("{ratio:.2}")
    } else {
        format!("{ratio:.0}")
    }
}

/// Human-readable latency: ms past a millisecond, else µs past a microsecond, else ns.
fn fmt_ns(ns: u128) -> String {
    if ns >= 1_000_000 {
        format!("{:.3} ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.2} us", ns as f64 / 1e3)
    } else {
        format!("{ns} ns")
    }
}

/// The self-timed median of `op` over `SELF_ITERS` samples, after two warm-up runs.
fn median_ns(mut op: impl FnMut()) -> u128 {
    op();
    op();
    let mut samples = Vec::with_capacity(SELF_ITERS);
    for _ in 0..SELF_ITERS {
        let start = Instant::now();
        op();
        samples.push(start.elapsed().as_nanos());
    }
    samples.sort_unstable();
    samples[samples.len() / 2]
}

/// Benchmark one class's backend path against its native-SQL twin: a `criterion`
/// group over both (so `cargo bench` reports each with `black_box`), then a
/// self-timed median pass whose numbers feed the ratio table.
fn bench_pair(
    c: &mut Criterion,
    rec: &mut Recorder,
    size: i64,
    class: &'static str,
    basis: Basis,
    mut backend: impl FnMut(),
    mut raw: impl FnMut(),
) {
    let mut group = c.benchmark_group(format!("n{size}/{class}"));
    group.bench_function("backend", |b| b.iter(&mut backend));
    group.bench_function("raw_sql", |b| b.iter(&mut raw));
    group.finish();

    let backend_ns = median_ns(&mut backend);
    let raw_ns = median_ns(&mut raw);
    rec.push(size, class, basis, backend_ns, raw_ns);
}

/// Every backend query pattern the matrix runs against `nodes`, with its planner
/// node-type list — printed at LARGE so the report can confirm the native baselines
/// ride indexes rather than an accidental seq scan.
fn report_plans(client: &mut Client, schema: &str) {
    let mid = SIZES.iter().copied().max().unwrap_or(0) / 2;
    let probes: [(&str, String); 5] = [
        (
            "point lookup",
            format!(
                "SELECT id, incarnation, value FROM {schema}.nodes \
                 WHERE parent_id = {SENTINEL} AND step_name = 'items' AND key_enc = int8send({mid}::int8) \
                 AND value IS NOT NULL"
            ),
        ),
        (
            "ordered scan",
            format!(
                "SELECT id, key_enc, value FROM {schema}.nodes \
                 WHERE parent_id = {SENTINEL} AND step_name = 'items' AND value IS NOT NULL ORDER BY key_enc"
            ),
        ),
        (
            "range scan",
            format!(
                "SELECT id, key_enc FROM {schema}.nodes \
                 WHERE parent_id = {SENTINEL} AND step_name = 'items' AND value IS NOT NULL \
                 AND key_enc >= int8send({mid}::int8) AND key_enc < int8send(({mid} + {RANGE_SPAN})::int8) \
                 ORDER BY key_enc"
            ),
        ),
        (
            "non-key filter",
            format!(
                "SELECT id FROM {schema}.nodes \
                 WHERE parent_id = {SENTINEL} AND step_name = 'items' AND value IS NOT NULL \
                 AND value->'st'->0->1->>'i' = '{FILTER_BUCKET}'"
            ),
        ),
        (
            "reference join",
            format!(
                "SELECT count(*) FROM {schema}.nodes i \
                 JOIN {schema}.nodes o ON o.parent_id = {SENTINEL} AND o.step_name = 'orgs' \
                   AND o.key_enc = int8send((i.value->'st'->2->1->>'i')::int8) \
                 WHERE i.parent_id = {SENTINEL} AND i.step_name = 'items' AND i.value IS NOT NULL"
            ),
        ),
    ];
    println!("\n----- native-SQL plans at LARGE (schema {schema}) -----");
    for (label, sql) in &probes {
        let types = plan_node_types(client, sql);
        println!("PLAN[{label:<15}] {types:?}");
    }
    println!("-------------------------------------------------------\n");
}

/// The `Node Type`s in the plan for `sql`, depth first — the same walk the
/// index-coverage gate uses, here for reporting rather than assertion.
fn plan_node_types(client: &mut Client, sql: &str) -> Vec<String> {
    let doc: serde_json::Value = client
        .query_one(&format!("EXPLAIN (FORMAT JSON) {sql}"), &[])
        .expect("EXPLAIN runs")
        .get(0);
    let mut out = Vec::new();
    if let Some(plan) = doc.get(0).and_then(|entry| entry.get("Plan")) {
        walk_plan(plan, &mut out);
    }
    out
}

fn walk_plan(plan: &serde_json::Value, out: &mut Vec<String>) {
    if let Some(kind) = plan.get("Node Type").and_then(serde_json::Value::as_str) {
        out.push(kind.to_owned());
    }
    if let Some(children) = plan.get("Plans").and_then(serde_json::Value::as_array) {
        for child in children {
            walk_plan(child, out);
        }
    }
}

fn matrix(c: &mut Criterion) {
    let handle = support::acquire();
    let mut factory = handle.factory("matrix");
    let mut rec = Recorder::new();
    let largest = SIZES.iter().copied().max().unwrap_or(0);

    for &size in SIZES {
        let (fixture, mut client) = Fixture::build(&mut factory, size);
        let Fixture { mut store, items, orgs, store_schema: s, twin_schema: rs, .. } = fixture;

        if size == largest {
            report_plans(&mut client, &s);
        }

        // Bind values shared by a class's backend path and its native twin.
        let mid = size / 2;
        let mid_addr = item_address(mid);
        let mid_enc = key_enc_be(mid);
        let range_lo = key_enc_be(mid);
        let range_hi = key_enc_be(mid + RANGE_SPAN);
        let filter_bucket_text = FILTER_BUCKET.to_string();

        // (1) SIMPLE — point lookup by key.
        let row_sql = format!(
            "SELECT id, incarnation, value FROM {s}.nodes \
             WHERE parent_id = {SENTINEL} AND step_name = 'items' AND key_enc = $1 AND value IS NOT NULL"
        );
        bench_pair(
            c,
            &mut rec,
            size,
            "1 point lookup",
            Basis::Projection,
            || {
                black_box(store.row(black_box(&mid_addr)).expect("row"));
            },
            || {
                black_box(client.query_opt(&row_sql, &[&mid_enc]).expect("raw point lookup"));
            },
        );

        // (2) SIMPLE — full ordered collection scan.
        let scan_sql = format!(
            "SELECT id, key_enc, incarnation, value FROM {s}.nodes \
             WHERE parent_id = {SENTINEL} AND step_name = 'items' AND value IS NOT NULL ORDER BY key_enc"
        );
        bench_pair(
            c,
            &mut rec,
            size,
            "2 ordered scan",
            Basis::Projection,
            || {
                black_box(store.scan(black_box(&items)).expect("scan"));
            },
            || {
                black_box(client.query(&scan_sql, &[]).expect("raw scan"));
            },
        );

        // (3) MODERATE — bounded key-range / prefix scan. The contract exposes no
        // range primitive, so the backend path scans the whole collection from the
        // projection and narrows in Rust; native rides the `key_enc` index range.
        let range_sql = format!(
            "SELECT id, key_enc, value FROM {s}.nodes \
             WHERE parent_id = {SENTINEL} AND step_name = 'items' AND value IS NOT NULL \
             AND key_enc >= $1 AND key_enc < $2 ORDER BY key_enc"
        );
        bench_pair(
            c,
            &mut rec,
            size,
            "3 range scan",
            Basis::Projection,
            || {
                let hits = store
                    .scan(&items)
                    .expect("scan")
                    .into_iter()
                    .filter(|(address, _)| {
                        addr_key_int(address).is_some_and(|k| k >= mid && k < mid + RANGE_SPAN)
                    })
                    .count();
                black_box(hits);
            },
            || {
                black_box(client.query(&range_sql, &[&range_lo, &range_hi]).expect("raw range"));
            },
        );

        // (4) COMPLEX — filter over a NON-key field (`bucket`). Backend scans + filters
        // in Rust; native applies a JSONB `WHERE` (no index on the field).
        let filter_sql = format!(
            "SELECT id FROM {s}.nodes \
             WHERE parent_id = {SENTINEL} AND step_name = 'items' AND value IS NOT NULL \
             AND value->'st'->0->1->>'i' = $1"
        );
        bench_pair(
            c,
            &mut rec,
            size,
            "4 non-key filter",
            Basis::Projection,
            || {
                let hits = store
                    .scan(&items)
                    .expect("scan")
                    .iter()
                    .filter(|(_, row)| struct_int_field(row, "bucket") == Some(FILTER_BUCKET))
                    .count();
                black_box(hits);
            },
            || {
                black_box(client.query(&filter_sql, &[&filter_bucket_text]).expect("raw filter"));
            },
        );

        // (5) COMPLEX — reference traversal / join across `items` → `orgs`: sum the
        // referenced org's `region` over every item. Backend builds an org map and
        // resolves each item's `org` field; native self-joins on `orgs.key_enc`.
        let join_sql = format!(
            "SELECT COALESCE(sum((o.value->'st'->1->1->>'i')::int8), 0) FROM {s}.nodes i \
             JOIN {s}.nodes o ON o.parent_id = {SENTINEL} AND o.step_name = 'orgs' \
               AND o.key_enc = int8send((i.value->'st'->2->1->>'i')::int8) \
             WHERE i.parent_id = {SENTINEL} AND i.step_name = 'items' AND i.value IS NOT NULL"
        );
        bench_pair(
            c,
            &mut rec,
            size,
            "5 reference join",
            Basis::Projection,
            || {
                let org_region: BTreeMap<i64, i64> = store
                    .scan(&orgs)
                    .expect("scan orgs")
                    .iter()
                    .filter_map(|(address, row)| {
                        Some((addr_key_int(address)?, struct_int_field(row, "region")?))
                    })
                    .collect();
                let sum: i64 = store
                    .scan(&items)
                    .expect("scan items")
                    .iter()
                    .filter_map(|(_, row)| org_region.get(&struct_int_field(row, "org")?))
                    .sum();
                black_box(sum);
            },
            || {
                black_box(client.query_one(&join_sql, &[]).expect("raw join"));
            },
        );

        // (6) COMPLEX — aggregate: count per `bucket`. Backend groups in Rust; native
        // `GROUP BY` the extracted bucket.
        let agg_sql = format!(
            "SELECT (value->'st'->0->1->>'i') AS bucket, count(*) FROM {s}.nodes \
             WHERE parent_id = {SENTINEL} AND step_name = 'items' AND value IS NOT NULL GROUP BY 1"
        );
        bench_pair(
            c,
            &mut rec,
            size,
            "6 aggregate group-by",
            Basis::Projection,
            || {
                let mut counts: BTreeMap<i64, u64> = BTreeMap::new();
                for (_, row) in store.scan(&items).expect("scan") {
                    if let Some(bucket) = struct_int_field(&row, "bucket") {
                        *counts.entry(bucket).or_default() += 1;
                    }
                }
                black_box(counts);
            },
            || {
                black_box(client.query(&agg_sql, &[]).expect("raw aggregate"));
            },
        );

        // (7) COMPLEX — evaluate a declared `$view`: the active-bucket items projected
        // to (key, label), in key order. Backend filters + projects over the ordered
        // projection scan; native filters + projects in SQL, index-ordered.
        let view_sql = format!(
            "SELECT key_enc, value->'st'->1->1->>'s' AS label FROM {s}.nodes \
             WHERE parent_id = {SENTINEL} AND step_name = 'items' AND value IS NOT NULL \
             AND value->'st'->0->1->>'i' = $1 ORDER BY key_enc"
        );
        bench_pair(
            c,
            &mut rec,
            size,
            "7 view filter+project",
            Basis::Projection,
            || {
                let projected: Vec<(i64, String)> = store
                    .scan(&items)
                    .expect("scan")
                    .iter()
                    .filter(|(_, row)| struct_int_field(row, "bucket") == Some(FILTER_BUCKET))
                    .filter_map(|(address, row)| {
                        Some((addr_key_int(address)?, struct_text_field(row, "label")?))
                    })
                    .collect();
                black_box(projected);
            },
            || {
                black_box(client.query(&view_sql, &[&filter_bucket_text]).expect("raw view"));
            },
        );

        // (8) WRITE — commit a single-row insert. The one like-for-like axis: the
        // backend's admission transaction vs. the identical statements by hand against
        // the same-size twin schema. Distinct keys per commit keep both gapless.
        let mut write_key: i64 = 0;
        let mut raw_seq: i64 = 0;
        bench_pair(
            c,
            &mut rec,
            size,
            "8 commit insert",
            Basis::LikeForLike,
            || {
                let key = write_key;
                write_key += 1;
                let mut txn = store.begin();
                txn.insert(write_address(key), Value::Text(Text::new(format!("w-{key}"))))
                    .expect("stage insert");
                black_box(txn.commit().expect("commit"));
            },
            || {
                raw_seq += 1;
                raw_commit(&mut client, &rs, raw_seq);
            },
        );
    }

    rec.report();
}

/// The admission transaction `PgStore::commit_transition` runs — head lock, log
/// append, node insert (a top-level `writes` row under the root sentinel), head bump
/// — issued by hand against the twin schema `rs`, so the write axis is like-for-like.
fn raw_commit(client: &mut Client, rs: &str, seq: i64) {
    let mut txn = client.transaction().expect("begin");
    txn.execute(&format!("SELECT head FROM {rs}.instance_meta WHERE id = 1 FOR UPDATE"), &[])
        .expect("lock head");
    txn.execute(
        &format!("INSERT INTO {rs}.commit_log (seq, transaction_id, ops) VALUES ($1, NULL, '[]'::jsonb)"),
        &[&seq],
    )
    .expect("append log");
    txn.execute(
        &format!(
            "INSERT INTO {rs}.nodes (parent_id, step_name, key_enc, key_wire, incarnation, value) \
             VALUES (0, 'writes', int8send($1::int8), \
                     jsonb_build_array(jsonb_build_object('i', $1::text)), $2::text, \
                     jsonb_build_object('s', $3::text))"
        ),
        &[&seq, &format!("inc-w-{seq}"), &format!("w-{seq}")],
    )
    .expect("insert node");
    txn.execute(&format!("UPDATE {rs}.instance_meta SET head = $1 WHERE id = 1"), &[&seq])
        .expect("bump head");
    txn.commit().expect("commit");
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_secs(1));
    targets = matrix
}
criterion_main!(benches);
