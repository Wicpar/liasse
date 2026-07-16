//! Model construction is the load-time hot path: parsing a definition into the
//! validated tree, typing every declared expression, and checking mutations and
//! surfaces. This benchmarks that end-to-end build over a representative
//! package (keyed collections, computed values, refs, a view, mutations, and a
//! public surface), and — separately — the parse step it builds on, so a
//! regression can be attributed to parsing versus semantic validation.

// A benchmark asserts its fixed input parses; a panic there is the intended
// failure mode, not a production path.
#![allow(clippy::expect_used)]

use criterion::{criterion_group, criterion_main, Criterion};
use liasse_diag::SourceMap;
use liasse_model::Model;
use liasse_syntax::parse_document;

const PACKAGE: &str = r#"{
  "$liasse": 1
  "$app": "bench.shop@1.4.2"
  "$semantics": { "timestamp_precision": "us" }
  "$model": {
    "accounts": {
      "$key": "id"
      "id": "uuid = uuid()"
      "email": {
        "$type": "text"
        "$normalize": "string.lower(string.trim(.))"
        "$check": ["size(.) > 0", "An email is required"]
      }
      "created_at": "timestamp = now()"
    }
    "products": {
      "$key": "id"
      "$unique": ["sku"]
      "id": "uuid = uuid()"
      "sku": "text"
      "net": "decimal"
      "tax": "decimal"
      "total": "= .net + .tax"
      "status": { "$enum": ["draft", "active", "closed"] }
    }
    "orders": {
      "$key": ["account", "seq"]
      "account": { "$ref": "/accounts" }
      "seq": "int"
      "note": "text?"
      "$mut": {
        "annotate({ note: text })": ".note = @note"
      }
    }
    "active_products": {
      "$view": ".products[:p | p.status == 'active'] { id, sku, total, $sort: [-total] }"
    }
    "$mut": {
      "add_product": "return .products + { sku: @sku, net: @net, tax: @tax }"
    }
    "$public": {
      "catalog": {
        "$view": ".active_products"
        "$mut": { "add": ".add_product" }
      }
    }
  }
}"#;

fn build_once() {
    let mut sources = SourceMap::new();
    let id = sources.add_file("bench.liasse", PACKAGE);
    let document = parse_document(id, PACKAGE).expect("definition parses");
    let _ = Model::build(&mut sources, id, &document);
}

fn parse_once() {
    let mut sources = SourceMap::new();
    let id = sources.add_file("bench.liasse", PACKAGE);
    let _ = parse_document(id, PACKAGE).expect("definition parses");
}

fn benches(c: &mut Criterion) {
    c.bench_function("model_build_full", |b| b.iter(build_once));
    c.bench_function("model_parse_only", |b| b.iter(parse_once));
}

criterion_group!(model, benches);
criterion_main!(model);
