#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Temporal selectors over bucketed views (§14.1, §14.2).
//!
//! `.$at(t)` selects rows active at `t`; `.$between(a, b)` selects rows whose
//! half-open interval intersects the non-empty range `[a, b)`; `.$all` selects
//! every extant row. Activity is resolved by the environment's temporal index
//! (a test index reading each row's `from`/`until` bound cells), so evaluation
//! stays pure. Expected row sets are deduced from §14.1's half-open rules.

mod common;

use common::{
    check, check_rejects, collection, eval, ids, keyless_row, row, row_type, scalar, scell,
    try_eval, vtext, view, FixedEnv, FixedScope,
};
use liasse_expr::{Cell, EvalError, ExprType, Row, RowType};
use liasse_value::{Precision, Timestamp, Type, Value};

fn ts(micros: i128) -> Value {
    Value::Timestamp(Timestamp::new(micros, Precision::Micros))
}

/// A bucketed session row with an optional `[from, until)` interval.
fn session(seed: u64, id: &str, from: Option<i128>, until: Option<i128>) -> Row {
    row(
        seed,
        vtext(id),
        vec![
            ("id", scell(vtext(id))),
            ("from", scell(from.map_or(Value::None, ts))),
            ("until", scell(until.map_or(Value::None, ts))),
        ],
    )
}

fn scope() -> FixedScope {
    let opt_ts = Type::Optional(Box::new(Type::timestamp()));
    let ty = row_type(
        vec![
            ("id", scalar(Type::Text)),
            ("from", scalar(opt_ts.clone())),
            ("until", scalar(opt_ts)),
        ],
        Some(scalar(Type::Text)),
    );
    let root_ty = row_type(vec![("sessions", view(ty))], None);
    FixedScope::new(ExprType::Row(root_ty))
        .param("t", scalar(Type::timestamp()))
        .param("a", scalar(Type::timestamp()))
        .param("b", scalar(Type::timestamp()))
        .param("bad", scalar(Type::Text))
}

fn world(rows: Vec<Row>) -> (Cell, Row) {
    let root = keyless_row(0, vec![("sessions", collection(rows))]);
    (Cell::Row(Box::new(root.clone())), root)
}

fn three_sessions() -> Vec<Row> {
    vec![
        session(1, "active", Some(0), Some(100)),
        session(2, "expired", Some(0), Some(50)),
        session(3, "future", Some(200), None),
    ]
}

/// §14.1: `.$at(t)` returns exactly the rows whose half-open interval contains
/// `t` — the started-and-not-yet-ended rows.
#[test]
fn at_selects_rows_active_at_instant() {
    let (dot, root) = world(three_sessions());
    let env = FixedEnv::new(root).param("t", scell(ts(60)));
    // t=60: active [0,100) ✓, expired [0,50) ended, future [200,∞) not started.
    let result = eval(&scope(), &env, &dot, ".sessions.$at(@t) { id }");
    assert_eq!(ids(&result, "id"), vec![vtext("active")]);
}

/// §14.1: `.$between(a, b)` returns the rows whose interval intersects `[a, b)`;
/// a bound touching only at an endpoint (half-open) does not intersect.
#[test]
fn between_selects_intersecting_intervals() {
    let (dot, root) = world(three_sessions());
    let env = FixedEnv::new(root).param("a", scell(ts(40))).param("b", scell(ts(70)));
    // [40,70): active [0,100) ✓ (0<70, 100>40); expired [0,50) ✓ (50>40);
    // future [200,∞) ✗ (200 !< 70).
    let result = eval(&scope(), &env, &dot, ".sessions.$between(@a, @b) { id }");
    assert_eq!(ids(&result, "id"), vec![vtext("active"), vtext("expired")]);
}

/// §14.1: an `until` exactly equal to the query's lower bound does not intersect
/// — `[from, until)` is half-open, so `until` is outside the interval.
#[test]
fn between_lower_bound_at_until_excludes() {
    let (dot, root) = world(vec![session(1, "s", Some(0), Some(50))]);
    let env = FixedEnv::new(root).param("a", scell(ts(50))).param("b", scell(ts(90)));
    // [50,90) starts exactly at s.until=50: 50 !> 50, no intersection.
    let result = eval(&scope(), &env, &dot, ".sessions.$between(@a, @b) { id }");
    assert!(ids(&result, "id").is_empty());
}

/// §14.2: `.$all` exposes every extant row independent of current activity,
/// including an already-expired one.
#[test]
fn all_exposes_every_extant_row() {
    let (dot, root) = world(three_sessions());
    let result = eval(&scope(), &FixedEnv::new(root), &dot, ".sessions.$all { id }");
    assert_eq!(
        ids(&result, "id"),
        vec![vtext("active"), vtext("expired"), vtext("future")],
    );
}

/// §14.1: `.$between(a, b)` requires `b > a`; a reversed range rejects
/// evaluation.
#[test]
fn between_reversed_range_rejects_evaluation() {
    let (dot, root) = world(three_sessions());
    let env = FixedEnv::new(root).param("a", scell(ts(70))).param("b", scell(ts(40)));
    let err = try_eval(&scope(), &env, &dot, ".sessions.$between(@a, @b) { id }");
    assert_eq!(err, Err(EvalError::EmptyTemporalRange));
}

/// §14.1: a temporal selector's instant must be a `timestamp`.
#[test]
fn at_rejects_non_timestamp_instant() {
    let diags = check_rejects(&scope(), ".sessions.$at(@bad) { id }");
    assert!(diags.iter().any(|d| d.message().contains("timestamp")));
}

/// §14.1: `.$between` takes exactly two instants.
#[test]
fn between_wrong_arity_rejects() {
    let diags = check_rejects(&scope(), ".sessions.$between(@a) { id }");
    assert!(diags.iter().any(|d| d.message().contains("argument")));
}

/// A temporal selector applies to a view, not a scalar.
#[test]
fn temporal_over_non_view_rejects() {
    let scope = FixedScope::new(ExprType::scalar(Type::Int)).param("t", scalar(Type::timestamp()));
    let diags = check_rejects(&scope, "1.$at(@t)");
    assert!(diags.iter().any(|d| d.message().contains("view")));
}

// --- Source-backed bucket structural bindings and the §14.5 enumeration guard ---

/// A source-backed bucket view field `periods`: one derived output field
/// (`credits`) plus the §14.4 structural bindings (`$from`/`$until`/`$index`).
/// `unbounded` marks a recurring series that may run forever (§14.5).
fn bucket_row(unbounded: bool) -> RowType {
    row_type(vec![("credits", scalar(Type::Decimal))], None)
        .with_structural(vec![
            ("from".to_owned(), scalar(Type::timestamp())),
            ("until".to_owned(), scalar(Type::Optional(Box::new(Type::timestamp())))),
            ("index".to_owned(), scalar(Type::Int)),
        ])
        .unbounded(unbounded)
}

fn bucket_scope(unbounded: bool) -> FixedScope {
    let root = row_type(vec![("periods", view(bucket_row(unbounded)))], None);
    FixedScope::new(ExprType::Row(root))
        .param("a", scalar(Type::timestamp()))
        .param("b", scalar(Type::timestamp()))
}

/// §14.4 — a projection over a temporal selector reads the derived output field
/// and the structural bindings, yielding a view of the projected output shape.
#[test]
fn projection_reads_bucket_structural_bindings() {
    let typed = check(&bucket_scope(false), ".periods.$all { i: $index, u: $until, credits }");
    let row = typed.ty().as_view().expect("a bucketed projection is a view");
    assert!(row.field("i").is_some());
    assert!(row.field("u").is_some());
    assert!(row.field("credits").is_some());
}

/// §14.5 — a bare projection over an unbounded recurring bucket is rejected: it
/// must be read through a bounded temporal selector first.
#[test]
fn unbounded_bare_projection_rejected() {
    let diags = check_rejects(&bucket_scope(true), ".periods { credits }");
    assert!(diags.iter().any(|d| d.message().contains("unbounded recurring bucket")));
}

/// §14.5 — `.$all` also enumerates the whole series and is rejected when unbounded.
#[test]
fn unbounded_all_rejected() {
    let diags = check_rejects(&bucket_scope(true), ".periods.$all { credits }");
    assert!(diags.iter().any(|d| d.message().contains("unbounded recurring bucket")));
}

/// §14.5 — a bounded window (`.$between`) lifts the guard, so projecting the
/// bounded slice type-checks even for an unbounded series.
#[test]
fn unbounded_bounded_selector_projection_types() {
    let typed = check(&bucket_scope(true), ".periods.$between(@a, @b) { i: $index, credits }");
    assert!(typed.ty().as_view().is_some());
}
