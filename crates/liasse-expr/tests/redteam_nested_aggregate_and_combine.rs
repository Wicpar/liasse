#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! RED TEAM — §7.5 aggregates over DERIVED views (the layer `redteam_aggregates`
//! and `views.rs` leave thin): an aggregate whose source is itself a grouped
//! projection carrying an aggregate output (a NESTED aggregate), an aggregate over
//! a `[:name | …]` filtered view, an aggregate over a projection whose field is a
//! computed expression, and set combinators over composite-keyed / tie-bearing
//! views. Every expectation is re-derived from §7.1–§7.5 + Annex B text alone and
//! is externally deducible without running the program:
//!
//!   lines: account a -> debits {10, 5}; b -> {3}; c -> {100}.
//!   grouped `total = sum(group.debit)` => a:15, b:3, c:100 (synthetic-key order).
//!   sum(totals)=118, max=100, min=3, avg=118/3, distinct={3,15,100}, count=3.
//!   filtered `debit > 4` keeps {10, 5, 100} => sum 115, min 5, max 100.
//!
//! §7.5: "sum(view.field) -> field numeric type ... avg converts every numeric
//! input exactly to decimal and performs decimal division under the package
//! semantics" (A.6: >=16 significant fractional digits, half-away-from-zero); Annex
//! B: min/max fold the total order, a set is B-ordered and de-duplicated by value.

mod common;

use std::collections::BTreeSet;

use common::{
    as_scalar, collection, eval, ids, row, row_type, rows_fields, scalar, scell, try_eval, vdec,
    vint, vtext, view, FixedEnv, FixedScope,
};
use liasse_expr::{Cell, ExprType, Row, RowType};
use liasse_value::{Type, Value};

/// One `lines` row: text key `id`, text `account`, int `debit`.
fn line(seed: u64, id: &str, account: &str, debit: i64) -> Row {
    row(
        seed,
        vtext(id),
        vec![
            ("id", scell(vtext(id))),
            ("account", scell(vtext(account))),
            ("debit", scell(vint(debit))),
        ],
    )
}

fn lines_type() -> RowType {
    row_type(
        vec![
            ("id", scalar(Type::Text)),
            ("account", scalar(Type::Text)),
            ("debit", scalar(Type::Int)),
        ],
        Some(scalar(Type::Text)),
    )
}

/// A scope/env whose root exposes the `lines` view.
fn lines(rows: Vec<Row>) -> (FixedScope, FixedEnv, Cell) {
    let root_ty = row_type(vec![("lines", view(lines_type()))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));
    let root = common::keyless_row(0, vec![("lines", collection(rows))]);
    let dot = Cell::Row(Box::new(root.clone()));
    (scope, FixedEnv::new(root), dot)
}

fn some_lines() -> Vec<Row> {
    vec![
        line(1, "l1", "a", 10),
        line(2, "l2", "a", 5),
        line(3, "l3", "b", 3),
        line(4, "l4", "c", 100),
    ]
}

/// The grouped-view text once, reused by every nested-aggregate case.
const GROUPED: &str = ".lines { $key: account, account, total: sum(group.debit) }";

#[test]
fn grouped_totals_are_the_externally_known_folds() {
    // Sanity floor: the grouped view itself must fold to a:15, b:3, c:100 in
    // synthetic-key ascending order, so the nested-aggregate expectations rest on a
    // verified base rather than the program's own answer.
    let (scope, env, dot) = lines(some_lines());
    let result = eval(&scope, &env, &dot, GROUPED);
    assert_eq!(
        rows_fields(&result),
        vec![
            vec![("account".to_owned(), vtext("a")), ("total".to_owned(), vint(15))],
            vec![("account".to_owned(), vtext("b")), ("total".to_owned(), vint(3))],
            vec![("account".to_owned(), vtext("c")), ("total".to_owned(), vint(100))],
        ]
    );
}

// ---- nested aggregate: aggregate OVER a grouped view's aggregate field -----

#[test]
fn sum_over_grouped_aggregate_field() {
    let (scope, env, dot) = lines(some_lines());
    // sum of {15, 3, 100} = 118.
    let src = format!("sum(({GROUPED}).total)");
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, &src)), vint(118));
}

#[test]
fn count_over_grouped_view_is_group_count() {
    let (scope, env, dot) = lines(some_lines());
    let src = format!("count({GROUPED})");
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, &src)), vint(3));
}

#[test]
fn min_max_over_grouped_aggregate_field() {
    let (scope, env, dot) = lines(some_lines());
    let min_src = format!("min(({GROUPED}).total)");
    let max_src = format!("max(({GROUPED}).total)");
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, &min_src)), vint(3));
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, &max_src)), vint(100));
}

#[test]
fn avg_over_grouped_aggregate_field_divides_exactly() {
    let (scope, env, dot) = lines(some_lines());
    // avg of {15, 3, 100} = 118/3 = 39.3333333333333333 (>=16 significant
    // fractional digits, half-away-from-zero at the 16th; magnitude >= 1 so scale 16).
    let src = format!("avg(({GROUPED}).total)");
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, &src)), vdec("39.3333333333333333"));
}

#[test]
fn distinct_over_grouped_aggregate_field() {
    let (scope, env, dot) = lines(some_lines());
    // distinct of {15, 3, 100} = the B-ordered set {3, 15, 100}.
    let src = format!("distinct(({GROUPED}).total)");
    let got = as_scalar(&eval(&scope, &env, &dot, &src));
    let expected: BTreeSet<Value> = [vint(3), vint(15), vint(100)].into_iter().collect();
    assert_eq!(got, Value::Set(expected));
}

// ---- aggregate over a filtered view (§6.4 `[:name | cond]`) ----------------

#[test]
fn sum_over_filtered_view() {
    let (scope, env, dot) = lines(some_lines());
    // debit > 4 keeps {10, 5, 100}; sum 115.
    assert_eq!(
        as_scalar(&eval(&scope, &env, &dot, "sum(.lines[:x | x.debit > 4].debit)")),
        vint(115)
    );
}

#[test]
fn min_max_over_filtered_view() {
    let (scope, env, dot) = lines(some_lines());
    assert_eq!(
        as_scalar(&eval(&scope, &env, &dot, "min(.lines[:x | x.debit > 4].debit)")),
        vint(5)
    );
    assert_eq!(
        as_scalar(&eval(&scope, &env, &dot, "max(.lines[:x | x.debit > 4].debit)")),
        vint(100)
    );
}

#[test]
fn count_over_filtered_view() {
    let (scope, env, dot) = lines(some_lines());
    // Three rows survive `debit > 4`.
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "count(.lines[:x | x.debit > 4])")), vint(3));
}

// ---- aggregate over a projection whose field is a computed expression ------

#[test]
fn sum_over_computed_projection_field() {
    let (scope, env, dot) = lines(some_lines());
    // `doubled = debit + debit` over every row: {20, 10, 6, 200}; sum 236.
    let src = "sum((.lines { id, doubled: debit + debit }).doubled)";
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, src)), vint(236));
}

#[test]
fn avg_over_filtered_then_projected_is_none_when_empty() {
    let (scope, env, dot) = lines(some_lines());
    // No row has debit > 1000, so the filtered view is empty and avg is `none` (§7.5).
    assert_eq!(
        as_scalar(&eval(&scope, &env, &dot, "avg(.lines[:x | x.debit > 1000].debit)")),
        Value::None
    );
    // Empty sum is the field's numeric zero (int 0), not none.
    assert_eq!(
        as_scalar(&eval(&scope, &env, &dot, "sum(.lines[:x | x.debit > 1000].debit)")),
        vint(0)
    );
}

// ---- combinators over derived views (§7.4) ---------------------------------

#[test]
fn union_of_two_filters_keeps_left_order_then_new_right() {
    let (scope, env, dot) = lines(some_lines());
    // left: debit >= 5 -> {l1, l2, l4}; right: account == "a" -> {l1, l2}.
    // §7.4 union: left order, then right identities not already present => l1,l2,l4.
    let src = r#".lines[:x | x.debit >= 5] { id } | .lines[:y | y.account == "a"] { id }"#;
    let result = eval(&scope, &env, &dot, src);
    assert_eq!(ids(&result, "id"), vec![vtext("l1"), vtext("l2"), vtext("l4")]);
}

#[test]
fn intersect_and_difference_by_identity() {
    let (scope, env, dot) = lines(some_lines());
    // left: debit >= 5 -> {l1,l2,l4}; right: account == "a" -> {l1,l2}.
    let inter = r#".lines[:x | x.debit >= 5] { id } & .lines[:y | y.account == "a"] { id }"#;
    let diff = r#".lines[:x | x.debit >= 5] { id } - .lines[:y | y.account == "a"] { id }"#;
    assert_eq!(ids(&eval(&scope, &env, &dot, inter), "id"), vec![vtext("l1"), vtext("l2")]);
    assert_eq!(ids(&eval(&scope, &env, &dot, diff), "id"), vec![vtext("l4")]);
}

#[test]
fn count_over_a_union_view() {
    let (scope, env, dot) = lines(some_lines());
    // The union above has three distinct identities.
    let src = r#"count(.lines[:x | x.debit >= 5] { id } | .lines[:y | y.account == "a"] { id })"#;
    // If either combinator or `count` double-counted a shared identity, this trips.
    match try_eval(&scope, &env, &dot, src) {
        Ok(cell) => assert_eq!(as_scalar(&cell), vint(3)),
        Err(err) => panic!("count over a union view errored: {}", err.message()),
    }
}
