//! Backend op vs. equivalent raw-SQL query, on one shared populated database.
//!
//! CLAUDE.md requires the PostgreSQL backend's overhead to sit *near* that of the
//! equivalent raw PostgreSQL request. These benchmarks make that gap measurable:
//! for each op the backend exposes they run the backend's own path against the raw
//! SQL a hand-written query would issue, over the same populated tables.
//!
//! The read ops (`row`, `scan`, `snapshot`, `get_blob`) are answered by `PgStore`
//! from its in-memory projection, so their raw-SQL counterparts (the indexed `nodes`
//! point lookup and ordered collection scan) measure the cost the projection saves
//! versus a round trip to PostgreSQL. The write op (`commit`) is SQL either way — the
//! backend's node-admission transaction versus the identical statements issued by
//! hand against an isolated twin schema — so that axis is a like-for-like overhead
//! measurement.
//!
//! These require a database, resolved (or bootstrapped) by the shared test
//! support module exactly as the integration tests do; `cargo bench --no-run` only
//! compiles them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

#[path = "../tests/support/mod.rs"]
mod support;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, RowAddress, StoreFactory, Transition,
};
use liasse_value::{Integer, Text, Value};
use postgres::Client;

use support::SchemaGuard;

/// Rows committed into the shared store before the read benchmarks run.
const POP: i64 = 2_000;

fn address(key: i64) -> RowAddress {
    RowAddress::root(AddressStep::new(
        NameSegment::new("items"),
        KeyValue::single(Value::Int(Integer::from(key))),
    ))
}

fn payload(key: i64) -> Value {
    Value::Text(Text::new(format!("row-{key:08}")))
}

fn backend_vs_raw(c: &mut Criterion) {
    let handle = support::acquire();
    let mut factory = handle.factory("bench");

    // The store under test, populated through the contract so its tables and its
    // projection agree.
    let instance = InstanceId::new("bench-store");
    let _store_guard = SchemaGuard::new(&factory, instance.clone());
    let mut store = factory.create(instance.clone()).expect("create store");
    for key in 0..POP {
        let mut txn = store.begin();
        if txn.insert(address(key), payload(key)).is_ok() {
            let _ = txn.commit();
        }
    }
    let blob_digest = store.put_blob(b"benchmark blob payload").expect("put blob");
    let head = store.head();

    // An isolated twin schema for the raw-SQL commit baseline, so hand-issued
    // writes never perturb the store under test.
    let raw_instance = InstanceId::new("bench-raw");
    let _raw_guard = SchemaGuard::new(&factory, raw_instance.clone());
    drop(factory.create(raw_instance.clone()).expect("create raw twin"));

    let schema = factory.schema_for(&instance);
    let raw_schema = factory.schema_for(&raw_instance);
    let s = schema.quoted();
    let rs = raw_schema.quoted();

    let mut client = factory.connect().expect("raw client");
    client
        .batch_execute(&format!("ANALYZE {s}.nodes; ANALYZE {s}.commit_log; ANALYZE {s}.blobs;"))
        .expect("analyze");

    // A representative `items` node's parent id and order-preserving key_enc, read
    // back so the raw-SQL point lookup and ordered scan bind exactly what the backend
    // wrote (the `int` keys' `key_enc` is opaque bytes this bench never reconstructs).
    let (lookup_parent, lookup_key_enc): (i64, Vec<u8>) = {
        let row = client
            .query_one(
                &format!(
                    "SELECT parent_id, key_enc FROM {s}.nodes \
                     WHERE step_name = 'items' ORDER BY key_enc OFFSET $1 LIMIT 1"
                ),
                &[&(POP / 2)],
            )
            .expect("pick a representative item node");
        (row.get(0), row.get(1))
    };

    // Axis 1: point lookup by (parent_id, step_name, key_enc).
    let lookup_addr = address(POP / 2);
    {
        let mut group = c.benchmark_group("key_lookup");
        group.bench_function("backend_projection", |b| {
            b.iter(|| black_box(store.row(black_box(&lookup_addr)).expect("row")));
        });
        group.bench_function("raw_sql", |b| {
            let sql = format!(
                "SELECT id, incarnation, value FROM {s}.nodes \
                 WHERE parent_id = $1 AND step_name = 'items' AND key_enc = $2"
            );
            b.iter(|| {
                black_box(client.query_opt(&sql, &[&lookup_parent, &lookup_key_enc]).expect("raw row"))
            });
        });
        group.finish();
    }

    // Axis 2: collection scan in Annex B key order (index-served on `key_enc`).
    let collection = CollectionPath::top(NameSegment::new("items"));
    {
        let mut group = c.benchmark_group("ordered_scan");
        group.bench_function("backend_projection", |b| {
            b.iter(|| black_box(store.scan(black_box(&collection)).expect("scan")));
        });
        group.bench_function("raw_sql", |b| {
            let sql = format!(
                "SELECT id, key_enc, incarnation, value FROM {s}.nodes \
                 WHERE parent_id = $1 AND step_name = 'items' ORDER BY key_enc"
            );
            b.iter(|| black_box(client.query(&sql, &[&lookup_parent]).expect("raw scan")));
        });
        group.finish();
    }

    // Axis 3: snapshot-at-frontier (fold the log up to head).
    let frontier_i64 = i64::try_from(head.get()).expect("head fits i64");
    {
        let mut group = c.benchmark_group("snapshot");
        group.bench_function("backend_projection", |b| {
            b.iter(|| black_box(store.snapshot(black_box(head)).expect("snapshot")));
        });
        group.bench_function("raw_sql", |b| {
            let sql = format!("SELECT seq, ops FROM {s}.commit_log WHERE seq <= $1 ORDER BY seq");
            b.iter(|| black_box(client.query(&sql, &[&frontier_i64]).expect("raw fold")));
        });
        group.finish();
    }

    // Axis 4: blob fetch by digest.
    let digest_text = blob_digest.to_canonical_text();
    {
        let mut group = c.benchmark_group("blob_get");
        group.bench_function("backend_projection", |b| {
            b.iter(|| black_box(store.get_blob(black_box(&blob_digest)).expect("get blob")));
        });
        group.bench_function("raw_sql", |b| {
            let sql = format!("SELECT bytes FROM {s}.blobs WHERE digest = $1");
            b.iter(|| black_box(client.query_opt(&sql, &[&digest_text]).expect("raw blob")));
        });
        group.finish();
    }

    // Axis 5: commit — the backend's admission transaction vs. the same three
    // statements issued by hand against the isolated twin schema.
    {
        let mut group = c.benchmark_group("commit");
        let mut back_key = POP;
        group.bench_function("backend", |b| {
            b.iter(|| {
                let mut txn = store.begin();
                txn.insert(address(back_key), payload(back_key)).expect("stage insert");
                let _ = txn.commit().expect("commit");
                back_key += 1;
            });
        });
        let mut raw_seq: i64 = 0;
        group.bench_function("raw_sql", |b| {
            b.iter(|| {
                raw_seq += 1;
                raw_commit(&mut client, &rs, raw_seq);
            });
        });
        group.finish();
    }
}

/// Issue the admission transaction `PgStore::commit_transition` runs — head lock,
/// log append, node insert (a top-level `items` row under the root sentinel), head
/// bump — by hand against `rs`.
fn raw_commit(client: &mut Client, rs: &str, seq: i64) {
    let mut txn = client.transaction().expect("begin");
    txn.execute(&format!("SELECT head FROM {rs}.instance_meta WHERE id = 1 FOR UPDATE"), &[])
        .expect("lock head");
    txn.execute(
        &format!("INSERT INTO {rs}.commit_log (seq, transaction_id, ops) VALUES ($1, NULL, '[]'::jsonb)"),
        &[&seq],
    )
    .expect("append log");
    let key_enc = seq.to_be_bytes().to_vec();
    txn.execute(
        &format!(
            "INSERT INTO {rs}.nodes (parent_id, step_name, key_enc, key_wire, incarnation, value) \
             VALUES (0, 'items', $1, '{{}}'::jsonb, $2, $3)"
        ),
        &[&key_enc, &format!("row-{seq}"), &serde_json::json!({"s": seq.to_string()})],
    )
    .expect("insert node");
    txn.execute(&format!("UPDATE {rs}.instance_meta SET head = $1 WHERE id = 1"), &[&seq])
        .expect("bump head");
    txn.commit().expect("commit");
}

criterion_group!(benches, backend_vs_raw);
criterion_main!(benches);
