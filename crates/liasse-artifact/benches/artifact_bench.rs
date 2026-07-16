//! Load-path microbenchmarks: composing an artifact and opening/verifying one.
//!
//! Non-trivial because both paths do real work over the whole archive — build
//! computes a SHA-256 per section and writes the ZIP64 container; open runs the
//! raw central-directory scan, re-hashes every referenced entry, and recursively
//! verifies nested children. These are hot on every export and every load, so
//! they are worth tracking; the digest and ZIP primitives themselves are not
//! benchmarked here (they belong to their own crates).

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

use liasse_artifact::{Artifact, ArtifactBuilder};
use liasse_ident::{HistoryPoint, InstanceId, LineageId, PointId};

fn point(lineage: &str, p: &str) -> HistoryPoint {
    HistoryPoint::new(LineageId::new(lineage), PointId::new(p))
}

/// A representative package: a definition, a non-trivial state section, a
/// history index, a handful of resources, and one nested child module.
fn representative_builder() -> ArtifactBuilder {
    let child = ArtifactBuilder::new(
        InstanceId::new("child-1"),
        point("clin-1", "cp1"),
        br#"{"$module":"vendor.feature@1.0.0","$liasse":1}"#.to_vec(),
        vec![0xA5; 4096],
        br#"{"format":1,"selected":{"lineage":"clin-1","point":"cp1"}}"#.to_vec(),
    )
    .build()
    .unwrap_or_default();

    let mut builder = ArtifactBuilder::new(
        InstanceId::new("inst-root"),
        point("lin-1", "p1"),
        br#"{"$app":"vendor.application@1.0.0","$liasse":1}"#.to_vec(),
        vec![0x5A; 64 * 1024],
        br#"{"format":1,"selected":{"lineage":"lin-1","point":"p1"}}"#.to_vec(),
    );
    for i in 0..8 {
        builder.section(
            format!("resources/asset-{i}.bin"),
            "application/octet-stream",
            vec![i as u8; 2048],
        );
    }
    builder.module("feature", InstanceId::new("child-1"), point("clin-1", "cp1"), child);
    builder
}

fn bench_build(c: &mut Criterion) {
    c.bench_function("build_representative", |b| {
        b.iter_batched(
            representative_builder,
            |builder| builder.build().unwrap_or_default(),
            BatchSize::SmallInput,
        );
    });
}

fn bench_open(c: &mut Criterion) {
    let bytes = representative_builder().build().unwrap_or_default();
    c.bench_function("open_and_verify_representative", |b| {
        b.iter(|| Artifact::open(&bytes).is_ok());
    });
}

criterion_group!(benches, bench_build, bench_open);
criterion_main!(benches);
