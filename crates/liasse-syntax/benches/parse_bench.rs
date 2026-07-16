//! Parse-throughput benchmarks on the axes Annex C's recursion makes
//! interesting: a broad package document, a deeply nested document, and a long
//! chained expression. The parser is a non-trivial custom abstraction over
//! pest, so both entry points are measured.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use liasse_diag::{SourceId, SourceMap};
use liasse_syntax::{parse_document, parse_expression, parse_type_expression};

/// A representative package document: several collections, fields, a view, and
/// a public surface (the §3.2 shape, widened).
fn package_document() -> String {
    let mut out =
        String::from("{\n  \"$liasse\": 1\n  \"$app\": \"bench.app@1.0.0\"\n  \"$model\": {\n");
    for i in 0..40 {
        out.push_str(&format!(
            "    \"coll_{i}\": {{ \"$key\": \"id\", \"id\": \"uuid = uuid()\", \"name\": \"text\", \"count\": \"int = 0\", \"done\": \"bool = false\" }}\n"
        ));
    }
    out.push_str("  }\n}\n");
    out
}

/// A document nested `depth` objects deep, stressing the recursive value rule.
fn nested_document(depth: usize) -> String {
    let mut out = String::new();
    for _ in 0..depth {
        out.push_str("{ \"child\": ");
    }
    out.push('1');
    for _ in 0..depth {
        out.push_str(" }");
    }
    out
}

/// A long left-leaning expression chain, stressing the postfix/operator layers.
fn long_expression(terms: usize) -> String {
    let mut out = String::from(".base");
    for i in 0..terms {
        out.push_str(&format!(".field_{i}[@k_{i}]"));
    }
    for i in 0..terms {
        out.push_str(&format!(" + .other_{i}"));
    }
    out
}

/// A wide struct type over nested generics, stressing the A.2 grammar's two
/// recursion axes (field lists and `wrapper<...>` nesting) together.
fn wide_type_expression(fields: usize) -> String {
    let mut out = String::from("{ ");
    for i in 0..fields {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("field_{i}?: optional<map<text, set<json>>>"));
    }
    out.push_str(" }");
    out
}

fn bench(c: &mut Criterion) {
    let package = package_document();
    let nested = nested_document(200);
    let expression = long_expression(60);

    let mut group = c.benchmark_group("parse");
    group.throughput(Throughput::Bytes(package.len() as u64));
    group.bench_with_input(BenchmarkId::new("document", "package"), &package, |b, src| {
        b.iter(|| run_document(src));
    });
    group.throughput(Throughput::Bytes(nested.len() as u64));
    group.bench_with_input(BenchmarkId::new("document", "nested"), &nested, |b, src| {
        b.iter(|| run_document(src));
    });
    group.throughput(Throughput::Bytes(expression.len() as u64));
    group.bench_with_input(
        BenchmarkId::new("expression", "long-chain"),
        &expression,
        |b, src| b.iter(|| run_expression(src)),
    );
    let type_expr = wide_type_expression(40);
    group.throughput(Throughput::Bytes(type_expr.len() as u64));
    group.bench_with_input(
        BenchmarkId::new("type-expression", "wide-struct"),
        &type_expr,
        |b, src| b.iter(|| run_type_expression(src)),
    );
    group.finish();
}

fn run_document(src: &str) {
    let (id, sources) = registered(src);
    let _ = sources;
    let _ = parse_document(id, src);
}

fn run_expression(src: &str) {
    let (id, sources) = registered(src);
    let _ = sources;
    let _ = parse_expression(id, src);
}

fn run_type_expression(src: &str) {
    let (id, sources) = registered(src);
    let _ = sources;
    let _ = parse_type_expression(id, src);
}

fn registered(src: &str) -> (SourceId, SourceMap) {
    let mut sources = SourceMap::new();
    let id = sources.add_label("bench", src);
    (id, sources)
}

criterion_group!(benches, bench);
criterion_main!(benches);
