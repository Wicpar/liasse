//! Pure-PG substrate: each contract read/commit vs the IDENTICAL hand-written SQL.
//!
//! Under the pure-PG re-architecture (`DESIGN-pure-pg.md`) `PgStore` holds **no
//! in-memory projection** — every contract read is one indexed SQL statement (or,
//! for `snapshot`, a `nodes` materialization / log fold) on a pooled connection.
//! AGENTS.md makes the backend's overhead over raw PostgreSQL a correctness concern,
//! so the axis here (§9) is **the contract read vs the identical hand-written SQL on
//! the same pooled substrate** — near *raw SQL*, not near RAM. The old
//! `projection vs SQL` framing measured the deleted `BTreeMap` projection and is
//! void; it is gone.
//!
//! The raw comparator issues the EXACT statement the backend's `read`/`store` path
//! issues (the chained-InitPlan point lookup, the ordered child scan, the
//! shape-directed recursive CTE, the head-materialization `nodes` scan, the
//! `commit_log` fold), on a dedicated connection to the same server/schema. The
//! delta backend−raw is therefore the backend's own cost: its pooled checkout plus
//! the typed decode into `StoredRow`/`Snapshot` (a warm pool checkout is sub-µs and
//! immaterial against a PostgreSQL round trip, so this is a faithful "overhead over
//! raw SQL"). `commit` is SQL either way — the admission transaction vs the identical
//! statements issued by hand against an isolated twin schema — so that axis is
//! like-for-like.
//!
//! What is NOT here: the filter/join/aggregate/`$view` matrix. That is Phase 10's
//! concern (the read-side expression pushdown through the `liasse` extension, which
//! does not exist yet); measuring it now would either re-measure the deleted
//! projection or a hydrate-then-Rust baseline this phase does not own.
//!
//! These require a database, resolved (or bootstrapped) by the shared test support
//! module exactly as the integration tests do; `cargo bench --no-run` only compiles.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

#[path = "../tests/support/mod.rs"]
mod support;

use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use data_encoding::HEXLOWER;
use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, CommitSeq, InstanceStore, KeyValue, RowAddress, Snapshot,
    StoreFactory, Transition,
};
use liasse_value::{Integer, Text, Value};
use postgres::Client;

use support::SchemaGuard;

/// Rows in the small collection scanned whole.
const SMALL: i64 = 64;
/// Rows in the large collection scanned whole.
const LARGE: i64 = 4_096;
/// Direct children of the subtree root reached by `scan_subtree`.
const KIDS: i64 = 1_000;
/// The two commit-history sizes for the `snapshot` crossover (fast path is O(state),
/// the log fold is O(history)); each store keeps state = 1 (one row updated N times)
/// so the fast path is flat while the fold grows with N. The 10x gap makes the
/// crossover legible; each real commit fsyncs the WAL (default `synchronous_commit`
/// on the disposable cluster), so a much larger N would dominate setup without
/// changing the *shape* the fast path stays flat while the fold scales linearly.
const HIST_SMALL: i64 = 1_000;
const HIST_LARGE: i64 = 10_000;
/// The `scan_subtree` depth guard the backend uses (`read.rs`), mirrored in the raw
/// recursive CTE so the two plans are identical.
const MAX_SUBTREE_DEPTH: i64 = 10_000;

fn step(name: &str, key: i64) -> AddressStep {
    AddressStep::new(NameSegment::new(name), KeyValue::single(Value::Int(Integer::from(key))))
}

fn text(payload: &str) -> Value {
    Value::Text(Text::new(payload.to_owned()))
}

/// The depth-`k` single-chain address `/d1/1/d2/1/…/dk/1` (every level a live row).
fn chain_addr(depth: usize) -> RowAddress {
    let names = chain_names(depth);
    let mut address = RowAddress::root(step(names[0], 1));
    for name in &names[1..] {
        address = address.child(step(name, 1));
    }
    address
}

/// The step names of the depth-`k` chain, `["d1", …, "dk"]`.
fn chain_names(depth: usize) -> Vec<&'static str> {
    ["d1", "d2", "d3", "d4", "d5"][..depth].to_vec()
}

/// Insert the depth-5 chain, the small/large collections, and the `scan_subtree`
/// tree into `store`, batching collection inserts to keep setup cheap.
fn populate_main<S: InstanceStore>(store: &mut S) {
    // The depth-5 chain, every level a live row so `row` reads at depth 1/3/5.
    let mut txn = store.begin();
    let leaf = chain_addr(5);
    let mut prefix: Vec<AddressStep> = Vec::new();
    for stepv in leaf.steps() {
        prefix.push(stepv.clone());
        let mut address = RowAddress::root(prefix[0].clone());
        for s in &prefix[1..] {
            address = address.child(s.clone());
        }
        txn.insert(address, text("chain")).unwrap();
    }
    txn.commit().unwrap();

    batch_insert(store, "small", SMALL);
    batch_insert(store, "large", LARGE);

    // The subtree: `/tree/1` with KIDS direct children under step `kids`.
    let mut txn = store.begin();
    txn.insert(RowAddress::root(step("tree", 1)), text("root")).unwrap();
    txn.commit().unwrap();
    let root = RowAddress::root(step("tree", 1));
    let mut key = 0;
    while key < KIDS {
        let mut txn = store.begin();
        for _ in 0..256.min(KIDS - key) {
            txn.insert(root.clone().child(step("kids", key)), text("kid")).unwrap();
            key += 1;
        }
        txn.commit().unwrap();
    }
}

/// Insert `count` rows into the top-level collection `name`, ~256 per commit.
fn batch_insert<S: InstanceStore>(store: &mut S, name: &'static str, count: i64) {
    let mut key = 0;
    while key < count {
        let mut txn = store.begin();
        for _ in 0..256.min(count - key) {
            txn.insert(RowAddress::root(step(name, key)), text("row")).unwrap();
            key += 1;
        }
        txn.commit().unwrap();
    }
}

/// Build a store whose history is `n` commits but whose state is one row (one insert,
/// then `n-1` updates) — the small-state / long-history shape that separates the
/// O(state) fast path from the O(history) fold.
fn build_history<S: InstanceStore>(store: &mut S, n: i64) {
    let address = RowAddress::root(step("hist", 1));
    let mut txn = store.begin();
    txn.insert(address.clone(), text("v0")).unwrap();
    txn.commit().unwrap();
    for i in 1..n {
        let mut txn = store.begin();
        txn.update(&address, text(&format!("v{i}"))).unwrap();
        txn.commit().unwrap();
    }
}

/// Resolve a single-chain address's per-level `(step_name, key_enc)` by walking DOWN
/// `(parent_id, step_name)` from the root sentinel (each level is unique here), so the
/// raw comparator can bind the exact `key_enc` bytes the backend wrote without the
/// crate's private encoder.
fn resolve_chain(raw: &mut Client, s: &str, names: &[&str]) -> Vec<(String, Vec<u8>)> {
    let mut parent = 0i64;
    let mut levels = Vec::new();
    for name in names {
        let row = raw
            .query_one(
                &format!("SELECT id, key_enc FROM {s}.nodes WHERE parent_id = $1 AND step_name = $2 LIMIT 1"),
                &[&parent, name],
            )
            .expect("resolve chain level");
        parent = row.get("id");
        levels.push(((*name).to_owned(), row.get::<_, Vec<u8>>("key_enc")));
    }
    levels
}

/// The EXACT §4.1 chained-InitPlan point lookup `read::row` builds, with each level's
/// `step_name`/`key_enc` inlined (controlled test values, no injection surface) and
/// the outermost `value IS NOT NULL` live gate.
fn point_lookup_sql(s: &str, levels: &[(String, Vec<u8>)]) -> String {
    let (final_level, ancestors) = levels.split_last().expect("non-empty address");
    let mut chain = "0".to_string();
    for (name, key) in ancestors {
        chain = format!(
            "(SELECT id FROM {s}.nodes WHERE parent_id = {chain} AND step_name = '{name}' AND key_enc = {})",
            bytea_literal(key)
        );
    }
    let (name, key) = final_level;
    format!(
        "SELECT incarnation, value FROM {s}.nodes \
         WHERE parent_id = {chain} AND step_name = '{name}' AND key_enc = {} AND value IS NOT NULL",
        bytea_literal(key)
    )
}

/// A PostgreSQL `bytea` hex literal, `'\xDEADBEEF'::bytea`.
fn bytea_literal(bytes: &[u8]) -> String {
    format!("'\\x{}'::bytea", HEXLOWER.encode(bytes))
}

fn substrate(c: &mut Criterion) {
    let handle = support::acquire();
    let mut factory = handle.factory("substrate");

    // ---- main store: chain + small + large + subtree ----
    let main_instance = InstanceId::new("bench-main");
    let _main_guard = SchemaGuard::new(&factory, main_instance.clone());
    let mut main = factory.create(main_instance.clone()).expect("create main store");
    populate_main(&mut main);
    let main_head = main.head().expect("main head");

    // History stores for the snapshot crossover (state = 1, history = N).
    let hist_small_instance = InstanceId::new("bench-hist-small");
    let _hs_guard = SchemaGuard::new(&factory, hist_small_instance.clone());
    let mut hist_small = factory.create(hist_small_instance.clone()).expect("create hist-small");
    build_history(&mut hist_small, HIST_SMALL);
    let hs_head = hist_small.head().expect("hist-small head");

    let hist_large_instance = InstanceId::new("bench-hist-large");
    let _hl_guard = SchemaGuard::new(&factory, hist_large_instance.clone());
    let mut hist_large = factory.create(hist_large_instance.clone()).expect("create hist-large");
    build_history(&mut hist_large, HIST_LARGE);
    let hl_head = hist_large.head().expect("hist-large head");

    // Isolated twin schema for the raw-SQL commit baseline.
    let raw_instance = InstanceId::new("bench-raw-twin");
    let _raw_guard = SchemaGuard::new(&factory, raw_instance.clone());
    drop(factory.create(raw_instance.clone()).expect("create raw twin"));

    let main_s = factory.schema_for(&main_instance).quoted();
    let hs_s = factory.schema_for(&hist_small_instance).quoted();
    let hl_s = factory.schema_for(&hist_large_instance).quoted();
    let raw_s = factory.schema_for(&raw_instance).quoted();

    let mut raw = factory.connect().expect("raw client");
    raw.batch_execute(&format!(
        "ANALYZE {main_s}.nodes; ANALYZE {hs_s}.commit_log; ANALYZE {hl_s}.commit_log;"
    ))
    .expect("analyze");

    // ---- row at depth 1/3/5: backend vs the identical chained-InitPlan lookup ----
    {
        let mut group = c.benchmark_group("row");
        for depth in [1usize, 3, 5] {
            let address = chain_addr(depth);
            let levels = resolve_chain(&mut raw, &main_s, &chain_names(depth));
            let sql = point_lookup_sql(&main_s, &levels);
            group.bench_function(format!("depth_{depth}/backend"), |b| {
                b.iter(|| black_box(main.row(black_box(&address)).expect("row")));
            });
            group.bench_function(format!("depth_{depth}/raw_sql"), |b| {
                b.iter(|| black_box(raw.query_opt(&sql, &[]).expect("raw row")));
            });
        }
        group.finish();
    }

    // ---- scan a whole collection in key order: backend vs the ordered child scan ----
    {
        let mut group = c.benchmark_group("scan");
        for (label, name) in [("small_64", "small"), ("large_4096", "large")] {
            let collection = CollectionPath::top(NameSegment::new(name));
            let sql = format!(
                "SELECT key_wire, incarnation, value FROM {main_s}.nodes \
                 WHERE parent_id = 0 AND step_name = '{name}' AND value IS NOT NULL ORDER BY key_enc"
            );
            group.bench_function(format!("{label}/backend"), |b| {
                b.iter(|| black_box(main.scan(black_box(&collection)).expect("scan")));
            });
            group.bench_function(format!("{label}/raw_sql"), |b| {
                b.iter(|| black_box(raw.query(&sql, &[]).expect("raw scan")));
            });
        }
        group.finish();
    }

    // ---- scan_subtree ~1000 nodes: backend vs the identical shape-directed CTE ----
    {
        let root = RowAddress::root(step("tree", 1));
        let steps = vec!["kids".to_owned()];
        let tree_levels = resolve_chain(&mut raw, &main_s, &["tree"]);
        let tree_key = bytea_literal(&tree_levels[0].1);
        let cte = format!(
            "WITH RECURSIVE sub AS ( \
               SELECT n.id, '[]'::jsonb AS rel_path, 0::bigint AS depth, n.incarnation, n.value \
               FROM {main_s}.nodes n \
               WHERE n.parent_id = 0 AND n.step_name = 'tree' AND n.key_enc = {tree_key} \
             UNION ALL \
               SELECT c.id, \
                      p.rel_path || jsonb_build_array(jsonb_build_array(to_jsonb(c.step_name), c.key_wire)), \
                      p.depth + 1, c.incarnation, c.value \
               FROM sub p \
               JOIN {main_s}.nodes c ON c.parent_id = p.id AND c.step_name = ANY(ARRAY['kids']::text[]) \
               WHERE p.depth < {MAX_SUBTREE_DEPTH} \
             ) \
             SELECT rel_path, depth, incarnation, value FROM sub WHERE depth > 0 AND value IS NOT NULL"
        );
        let mut group = c.benchmark_group("scan_subtree");
        group.bench_function("kids_1000/backend", |b| {
            b.iter(|| black_box(main.scan_subtree(black_box(&root), black_box(&steps)).expect("subtree")));
        });
        group.bench_function("kids_1000/raw_sql", |b| {
            b.iter(|| black_box(raw.query(&cte, &[]).expect("raw subtree")));
        });
        group.finish();
    }

    // ---- head: backend vs the single-row instance_meta read ----
    {
        let sql = format!("SELECT head FROM {main_s}.instance_meta WHERE id = 1");
        let mut group = c.benchmark_group("head");
        group.bench_function("backend", |b| b.iter(|| black_box(main.head().expect("head"))));
        group.bench_function("raw_sql", |b| {
            b.iter(|| black_box(raw.query_one(&sql, &[]).expect("raw head")));
        });
        group.finish();
    }

    // ---- snapshot(head): the head fast path (O(state)) vs the log fold (O(history)) ----
    for (store, store_head, s, sample) in [
        (&hist_small, hs_head, hs_s.as_str(), 30usize),
        (&hist_large, hl_head, hl_s.as_str(), 20usize),
    ] {
        let n = store_head.get();
        let head_i64 = i64::try_from(n).expect("head fits i64");
        let nodes_scan = format!(
            "SELECT id, parent_id, step_name, key_wire, incarnation, value FROM {s}.nodes"
        );
        let log_scan =
            format!("SELECT seq, transaction_id, ops FROM {s}.commit_log WHERE seq <= $1 ORDER BY seq");

        let mut group = c.benchmark_group(format!("snapshot/hist_{n}"));
        group.sample_size(sample);
        // Backend fast path — materialize head state from `nodes`, O(state).
        group.bench_function("head_fast_path", |b| {
            b.iter(|| black_box(store.snapshot(black_box(store_head)).expect("fast path")));
        });
        // Backend log fold — the O(history) replay the fast path replaces.
        group.bench_function("log_fold", |b| {
            b.iter(|| {
                let log = store.log_from(CommitSeq::GENESIS).expect("log");
                black_box(Snapshot::materialize(&log, store_head).expect("fold"))
            });
        });
        // Raw substrate under each: the `nodes` full scan (fast path) and the
        // `commit_log` prefix read (log fold), no typed decode.
        group.bench_function("raw_nodes_scan", |b| {
            b.iter(|| black_box(raw.query(&nodes_scan, &[]).expect("raw nodes scan")));
        });
        group.bench_function("raw_commit_log", |b| {
            b.iter(|| black_box(raw.query(&log_scan, &[&head_i64]).expect("raw log scan")));
        });
        group.finish();
    }

    // ---- commit: backend admission transaction vs the identical hand-issued SQL ----
    {
        let mut group = c.benchmark_group("commit");
        let mut back_key = LARGE;
        group.bench_function("backend", |b| {
            b.iter(|| {
                let mut txn = main.begin();
                txn.insert(RowAddress::root(step("large", back_key)), text("row")).expect("stage");
                let _ = txn.commit().expect("commit");
                back_key += 1;
            });
        });
        let mut raw_seq: i64 = 0;
        group.bench_function("raw_sql", |b| {
            b.iter(|| {
                raw_seq += 1;
                raw_commit(&mut raw, &raw_s, raw_seq);
            });
        });
        group.finish();

        // Keep the reference to `main_head` meaningful for readers of the report.
        let _ = main_head;
    }
}

/// The admission transaction `PgStore::commit_transition` runs — head lock, log
/// append, node insert (a top-level row under the sentinel), head bump — by hand
/// against the twin schema `raw_s`.
fn raw_commit(client: &mut Client, raw_s: &str, seq: i64) {
    let mut txn = client.transaction().expect("begin");
    txn.execute(&format!("SELECT head FROM {raw_s}.instance_meta WHERE id = 1 FOR UPDATE"), &[])
        .expect("lock head");
    txn.execute(
        &format!("INSERT INTO {raw_s}.commit_log (seq, transaction_id, ops) VALUES ($1, NULL, '[]'::jsonb)"),
        &[&seq],
    )
    .expect("append log");
    let key_enc = seq.to_be_bytes().to_vec();
    txn.execute(
        &format!(
            "INSERT INTO {raw_s}.nodes (parent_id, step_name, key_enc, key_wire, incarnation, value) \
             VALUES (0, 'items', $1, '{{}}'::jsonb, $2, $3)"
        ),
        &[&key_enc, &format!("row-{seq}"), &serde_json::json!({"s": seq.to_string()})],
    )
    .expect("insert node");
    txn.execute(&format!("UPDATE {raw_s}.instance_meta SET head = $1 WHERE id = 1"), &[&seq])
        .expect("bump head");
    txn.commit().expect("commit");
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(30)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(3));
    targets = substrate
}
criterion_main!(benches);
