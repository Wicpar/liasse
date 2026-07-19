#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Benchmarks for the two axes the runtime hammers on the reference store: the
//! staged-commit path (admit a transition and take the next serial position) and
//! the frontier snapshot scan (fold the log into current state and walk a
//! collection in Annex B order).

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, MemoryStore, RowAddress, Transition,
};
use liasse_value::{Integer, Text, Value};

fn address(key: i64) -> RowAddress {
    RowAddress::root(AddressStep::new(
        NameSegment::new("items"),
        KeyValue::single(Value::Int(Integer::from(key))),
    ))
}

fn payload(key: i64) -> Value {
    Value::Text(Text::new(format!("row-{key:08}")))
}

/// A store pre-populated with `rows` committed rows, one row per commit, so the
/// commit log has `rows` transitions for the snapshot fold to replay.
fn populated(rows: i64) -> MemoryStore {
    let mut store = MemoryStore::new(InstanceId::new("bench"));
    for key in 0..rows {
        let mut txn = store.begin();
        if txn.insert(address(key), payload(key)).is_err() {
            continue;
        }
        let _ = txn.commit();
    }
    store
}

fn staged_commit_throughput(c: &mut Criterion) {
    c.bench_function("staged_commit_insert", |b| {
        b.iter_batched(
            || populated(1_000),
            |mut store| {
                let mut txn = store.begin();
                let _ = txn.insert(address(1_000_000), payload(1_000_000));
                black_box(txn.commit().is_ok());
            },
            BatchSize::SmallInput,
        );
    });
}

fn snapshot_scan(c: &mut Criterion) {
    let store = populated(5_000);
    let head = store.head().expect("head");
    let collection = CollectionPath::top(NameSegment::new("items"));
    c.bench_function("snapshot_scan_5000", |b| {
        b.iter(|| {
            if let Ok(snapshot) = store.snapshot(black_box(head)) {
                black_box(snapshot.scan(&collection).len());
            }
        });
    });
}

criterion_group!(benches, staged_commit_throughput, snapshot_scan);
criterion_main!(benches);
