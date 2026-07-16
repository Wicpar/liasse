#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Criterion benches for the two axes the runtime hammers: evaluating a
//! filtered + projected + sorted view over a 10k-row collection, and
//! type-checking a large expression.

use std::collections::BTreeMap;
use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use liasse_diag::{SourceId, SourceMap};
use liasse_expr::{
    CallSite, Cell, Environment, ExprType, Row, RowId, RowType, Scope, TypedExpr,
};
use liasse_syntax::{parse_expression, SpannedExpression};
use liasse_value::{Integer, Precision, Text, Timestamp, Type, Uuid, Value};

struct BenchScope(ExprType);
impl Scope for BenchScope {
    fn current(&self) -> Option<ExprType> {
        Some(self.0.clone())
    }
    fn parent(&self, _: u32) -> Option<ExprType> {
        None
    }
    fn root(&self) -> Option<ExprType> {
        Some(self.0.clone())
    }
    fn param(&self, _: &str) -> Option<ExprType> {
        None
    }
    fn structural(&self, _: &str) -> Option<ExprType> {
        None
    }
    fn import(&self, _: &str) -> Option<ExprType> {
        None
    }
    fn binding(&self, _: &str) -> Option<ExprType> {
        None
    }
}

struct BenchEnv(Row);
impl Environment for BenchEnv {
    fn root(&self) -> &Row {
        &self.0
    }
    fn param(&self, _: &str) -> Option<Cell> {
        None
    }
    fn structural(&self, _: &str) -> Option<Cell> {
        None
    }
    fn import(&self, _: &str) -> Option<Cell> {
        None
    }
    fn now(&self) -> Timestamp {
        Timestamp::new(0, Precision::Micros)
    }
    fn uuid(&self, _: CallSite) -> Uuid {
        Uuid::from_bytes([0; 16])
    }
}

fn scalar(ty: Type) -> ExprType {
    ExprType::scalar(ty)
}

fn item_row_type() -> RowType {
    RowType::new(
        [
            ("id".to_owned(), scalar(Type::Text)),
            ("kind".to_owned(), scalar(Type::Text)),
            ("amount".to_owned(), scalar(Type::Int)),
        ],
        Some(scalar(Type::Text)),
    )
}

fn ten_thousand_rows() -> Cell {
    let rows = (0..10_000u64)
        .map(|n| {
            let key = Value::Text(Text::new(format!("k{n:05}")));
            let kind = if n % 2 == 0 { "even" } else { "odd" };
            let cells: BTreeMap<String, Cell> = [
                ("id".to_owned(), Cell::Scalar(key.clone())),
                ("kind".to_owned(), Cell::Scalar(Value::Text(Text::new(kind)))),
                (
                    "amount".to_owned(),
                    Cell::Scalar(Value::Int(Integer::from((10_000 - n) as i64))),
                ),
            ]
            .into_iter()
            .collect();
            Row::new(RowId::leaf(n), key, cells)
        })
        .collect();
    Cell::Collection(rows)
}

fn root_scope_env() -> (BenchScope, BenchEnv, Cell) {
    let root_ty = RowType::new([("items".to_owned(), ExprType::View(item_row_type()))], None);
    let scope = BenchScope(ExprType::Row(root_ty));
    let root = Row::keyless(RowId::leaf(0), [("items".to_owned(), ten_thousand_rows())]);
    let dot = Cell::Row(Box::new(root.clone()));
    (scope, BenchEnv(root), dot)
}

fn parse(source: &str) -> (SourceId, SourceMap, SpannedExpression) {
    let mut sources = SourceMap::new();
    let id = sources.add_label("bench", source);
    let parsed = parse_expression(id, source).expect("parse");
    (id, sources, parsed)
}

fn bench_view_evaluation(c: &mut Criterion) {
    let (scope, env, dot) = root_scope_env();
    let source =
        r#".items[:it | it.kind == "even"] { id, amount, $sort: [-amount], $limit: 100 }"#;
    let (id, _sources, parsed) = parse(source);
    let typed: TypedExpr = liasse_expr::check_statement(&scope, id, &parsed).expect("check");
    c.bench_function("evaluate_filter_project_sort_10k", |b| {
        b.iter(|| black_box(typed.evaluate(&env, black_box(&dot)).expect("eval")));
    });
}

fn bench_large_typing(c: &mut Criterion) {
    let (scope, _env, _dot) = root_scope_env();
    // A large additive chain — the checker walks a wide arithmetic tree.
    let mut source = String::from("1");
    for n in 0..400 {
        source.push_str(&format!(" + {n} * 2 - {n}"));
    }
    let (id, _sources, parsed) = parse(&source);
    c.bench_function("check_large_arithmetic_expression", |b| {
        b.iter(|| {
            black_box(liasse_expr::check_statement(&scope, id, black_box(&parsed)).expect("check"))
        });
    });
}

criterion_group!(benches, bench_view_evaluation, bench_large_typing);
criterion_main!(benches);
