#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! RED TEAM — §7.5 aggregate evaluation, the surface the existing suite barely
//! touches (only a grouped `sum` and one `avg` scale case). Every expectation is
//! re-derived from §7.5 text alone:
//!
//!   "count(view) -> int, sum(view.field) -> field numeric type,
//!    avg(view.field) -> decimal?, min/max(view.field) -> field type?,
//!    distinct(view.field) -> set<field type>.
//!    Absent inputs are skipped. Empty input yields 0 for count, numeric zero for
//!    sum, and none for avg, min, and max. avg converts every numeric input
//!    exactly to decimal and performs decimal division under the package
//!    semantics."
//!
//! and Annex B (min/max fold through the Annex B total order; a `set` is B-ordered
//! and de-duplicated by value identity). None of these outcomes reads the
//! program's own answer — they are externally deducible from the spec.

mod common;

use std::collections::BTreeSet;

use common::{as_scalar, collection, eval, row, row_type, scalar, scell, try_eval, view, FixedEnv, FixedScope};
use liasse_expr::{Cell, ExprType, Row, RowType};
use liasse_value::{Decimal, Integer, Text, Type, Value};

fn vint(v: i64) -> Value {
    Value::Int(Integer::from(v))
}
fn vdec(text: &str) -> Value {
    Value::Decimal(Decimal::parse(text).expect("decimal literal"))
}
fn vtext(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// The `items` row type: a text key `id`, an `int` amount, a `decimal` price, and
/// an `optional<int>` maybe (so absent-skipping is testable).
fn items_type() -> RowType {
    row_type(
        vec![
            ("id", scalar(Type::Text)),
            ("amount", scalar(Type::Int)),
            ("price", scalar(Type::Decimal)),
            ("maybe", scalar(Type::Optional(Box::new(Type::Int)))),
        ],
        Some(scalar(Type::Text)),
    )
}

/// One `items` row.
fn item(seed: u64, id: &str, amount: i64, price: &str, maybe: Option<i64>) -> Row {
    row(
        seed,
        vtext(id),
        vec![
            ("id", scell(vtext(id))),
            ("amount", scell(vint(amount))),
            ("price", scell(vdec(price))),
            ("maybe", scell(maybe.map_or(Value::None, vint))),
        ],
    )
}

/// A scope/env whose root exposes the `items` view built from `rows`.
fn items(rows: Vec<Row>) -> (FixedScope, FixedEnv, Cell) {
    let root_ty = row_type(vec![("items", view(items_type()))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));
    let root = common::keyless_row(0, vec![("items", collection(rows))]);
    let dot = Cell::Row(Box::new(root.clone()));
    (scope, FixedEnv::new(root), dot)
}

fn some() -> Vec<Row> {
    vec![
        item(1, "a", 3, "1.5", Some(10)),
        item(2, "b", 5, "2.5", None),
        item(3, "c", 2, "2.0", Some(40)),
    ]
}

// ---- count ----------------------------------------------------------------

#[test]
fn count_is_the_row_count() {
    let (scope, env, dot) = items(some());
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "count(.items)")), vint(3));
}

#[test]
fn count_of_empty_is_zero() {
    let (scope, env, dot) = items(vec![]);
    // §7.5: empty input yields 0 for count.
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "count(.items)")), vint(0));
}

// ---- sum ------------------------------------------------------------------

#[test]
fn sum_of_int_field() {
    let (scope, env, dot) = items(some());
    // 3 + 5 + 2 = 10, an int (field numeric type).
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "sum(.items.amount)")), vint(10));
}

#[test]
fn sum_of_decimal_field() {
    let (scope, env, dot) = items(some());
    // 1.5 + 2.5 + 2.0 = 6.0, a decimal (numerically 6).
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "sum(.items.price)")), vdec("6.0"));
}

#[test]
fn sum_skips_absent_optional_inputs() {
    let (scope, env, dot) = items(some());
    // §7.5: absent inputs are skipped. maybe = [10, none, 40] => 50.
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "sum(.items.maybe)")), vint(50));
}

#[test]
fn empty_int_sum_is_int_zero() {
    let (scope, env, dot) = items(vec![]);
    // §7.5: numeric zero for an empty sum; the field is `int`, so int 0.
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "sum(.items.amount)")), vint(0));
}

#[test]
fn empty_decimal_sum_is_decimal_zero() {
    let (scope, env, dot) = items(vec![]);
    // §7.5: numeric zero of the field's numeric type; the field is `decimal`.
    let got = as_scalar(&eval(&scope, &env, &dot, "sum(.items.price)"));
    assert_eq!(got, vdec("0"), "empty decimal sum must be a decimal zero");
    // The numeric type must be `decimal`, not `int` — a decimal zero, not int 0.
    assert!(matches!(got, Value::Decimal(_)), "empty decimal sum must be a Decimal, got {got:?}");
}

// ---- avg ------------------------------------------------------------------

#[test]
fn avg_divides_exactly() {
    let (scope, env, dot) = items(some());
    // (3 + 5 + 2) / 3 = 10/3 = 3.3333333333333333 (>=16 significant fractional
    // digits, half-away-from-zero at the 16th).
    assert_eq!(
        as_scalar(&eval(&scope, &env, &dot, "avg(.items.amount)")),
        vdec("3.3333333333333333")
    );
}

#[test]
fn avg_of_exact_quotient_normalizes() {
    let rows = vec![item(1, "a", 2, "0", None), item(2, "b", 4, "0", None)];
    let (scope, env, dot) = items(rows);
    // 6/2 = 3 exactly.
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "avg(.items.amount)")), vdec("3"));
}

#[test]
fn empty_avg_is_none() {
    let (scope, env, dot) = items(vec![]);
    // §7.5: empty input yields none for avg.
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "avg(.items.amount)")), Value::None);
}

#[test]
fn avg_skips_absent_inputs() {
    let (scope, env, dot) = items(some());
    // maybe present = [10, 40], mean = 25.
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "avg(.items.maybe)")), vdec("25"));
}

// ---- min / max ------------------------------------------------------------

#[test]
fn min_and_max_fold_through_annex_b_order() {
    let (scope, env, dot) = items(some());
    // amount = [3, 5, 2]; min 2, max 5.
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "min(.items.amount)")), vint(2));
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "max(.items.amount)")), vint(5));
}

#[test]
fn min_and_max_skip_absent_inputs() {
    let (scope, env, dot) = items(some());
    // maybe present = [10, 40].
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "min(.items.maybe)")), vint(10));
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "max(.items.maybe)")), vint(40));
}

#[test]
fn empty_min_and_max_are_none() {
    let (scope, env, dot) = items(vec![]);
    // §7.5: empty input yields none for min and max.
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "min(.items.amount)")), Value::None);
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "max(.items.amount)")), Value::None);
}

#[test]
fn all_absent_min_max_sum_avg() {
    // Every `maybe` absent: min/max/avg are none, sum is int zero.
    let rows = vec![item(1, "a", 0, "0", None), item(2, "b", 0, "0", None)];
    let (scope, env, dot) = items(rows);
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "min(.items.maybe)")), Value::None);
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "max(.items.maybe)")), Value::None);
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "avg(.items.maybe)")), Value::None);
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "sum(.items.maybe)")), vint(0));
}

// ---- distinct -------------------------------------------------------------

#[test]
fn distinct_dedups_by_value_identity() {
    let rows = vec![
        item(1, "a", 3, "0", None),
        item(2, "b", 3, "0", None),
        item(3, "c", 5, "0", None),
    ];
    let (scope, env, dot) = items(rows);
    // distinct(amount) = {3, 5} (a B-ordered set<int>).
    let got = as_scalar(&eval(&scope, &env, &dot, "distinct(.items.amount)"));
    let expected: BTreeSet<Value> = [vint(3), vint(5)].into_iter().collect();
    assert_eq!(got, Value::Set(expected));
}

#[test]
fn distinct_skips_absent_inputs() {
    let (scope, env, dot) = items(some());
    // maybe present = [10, 40] => {10, 40}.
    let got = as_scalar(&eval(&scope, &env, &dot, "distinct(.items.maybe)"));
    let expected: BTreeSet<Value> = [vint(10), vint(40)].into_iter().collect();
    assert_eq!(got, Value::Set(expected));
}

#[test]
fn distinct_of_all_absent_is_empty_set() {
    let rows = vec![item(1, "a", 0, "0", None), item(2, "b", 0, "0", None)];
    let (scope, env, dot) = items(rows);
    let got = as_scalar(&eval(&scope, &env, &dot, "distinct(.items.maybe)"));
    assert_eq!(got, Value::Set(BTreeSet::new()));
}

// A tripwire so a mis-typed source surfaces loudly instead of silently.
#[test]
fn aggregates_over_a_present_view_do_not_error() {
    let (scope, env, dot) = items(some());
    for src in ["count(.items)", "sum(.items.amount)", "avg(.items.price)", "min(.items.amount)", "max(.items.price)", "distinct(.items.amount)"] {
        assert!(try_eval(&scope, &env, &dot, src).is_ok(), "{src} unexpectedly errored");
    }
}
