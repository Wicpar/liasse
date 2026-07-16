#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Meter pool source views: the `$quantity` projection directive (§15.1).
//!
//! A pool source view assigns the structural `$quantity` role to a capacity
//! expression. The projected row exposes a `$quantity` cell the runtime
//! allocates against; a non-numeric capacity is rejected statically. Expected
//! values are deduced from §15.1 ("exact decimal quantities").

mod common;

use common::{
    check_rejects, collection, eval, keyless_row, row, row_type, rows_fields, scalar, scell, vdec,
    vint, vtext, view, FixedEnv, FixedScope,
};
use liasse_expr::{Cell, ExprType, Row};
use liasse_value::Type;

fn topups(rows: Vec<Row>) -> (FixedScope, FixedEnv, Cell) {
    let ty = row_type(
        vec![("id", scalar(Type::Text)), ("amount", scalar(Type::Decimal))],
        Some(scalar(Type::Text)),
    );
    let root_ty = row_type(vec![("topups", view(ty))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));
    let root = keyless_row(0, vec![("topups", collection(rows))]);
    let dot = Cell::Row(Box::new(root.clone()));
    (scope, FixedEnv::new(root), dot)
}

fn topup(seed: u64, id: &str, amount: &str) -> Row {
    row(seed, vtext(id), vec![("id", scell(vtext(id))), ("amount", scell(vdec(amount)))])
}

/// §15.1: `$quantity` projects the pool capacity into a structural `$quantity`
/// cell alongside the ordinary outputs.
#[test]
fn quantity_directive_projects_pool_capacity() {
    let (scope, env, dot) = topups(vec![topup(1, "t1", "100"), topup(2, "t2", "40")]);
    let result = eval(&scope, &env, &dot, ".topups { id, $quantity: .amount }");
    assert_eq!(
        rows_fields(&result),
        vec![
            vec![("$quantity".to_owned(), vdec("100")), ("id".to_owned(), vtext("t1"))],
            vec![("$quantity".to_owned(), vdec("40")), ("id".to_owned(), vtext("t2"))],
        ],
    );
}

/// §15.1: `$quantity` alone is a valid pool projection — the capacity is the
/// only structural output.
#[test]
fn quantity_directive_is_the_only_output() {
    let (scope, env, dot) = topups(vec![topup(1, "t1", "100")]);
    let result = eval(&scope, &env, &dot, ".topups { $quantity: .amount }");
    assert_eq!(
        rows_fields(&result),
        vec![vec![("$quantity".to_owned(), vdec("100"))]],
    );
}

/// §15.1: a pool capacity is a numeric quantity; a non-numeric `$quantity` is a
/// static type error.
#[test]
fn non_numeric_quantity_rejected() {
    let (scope, _, _) = topups(vec![]);
    let diags = check_rejects(&scope, ".topups { $quantity: id }");
    assert!(diags.iter().any(|d| d.message().contains("$quantity")));
}

/// §15.1: an `int` capacity is accepted (a numeric quantity), evaluating to the
/// integer value the runtime treats as an exact quantity.
#[test]
fn integer_quantity_accepted() {
    let ty = row_type(
        vec![("id", scalar(Type::Text)), ("units", scalar(Type::Int))],
        Some(scalar(Type::Text)),
    );
    let root_ty = row_type(vec![("grants", view(ty))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));
    let rows = vec![row(1, vtext("g1"), vec![("id", scell(vtext("g1"))), ("units", scell(vint(7)))])];
    let root = keyless_row(0, vec![("grants", collection(rows))]);
    let dot = Cell::Row(Box::new(root.clone()));
    let env = FixedEnv::new(root);
    let result = eval(&scope, &env, &dot, ".grants { $quantity: .units }");
    assert_eq!(rows_fields(&result), vec![vec![("$quantity".to_owned(), vint(7))]]);
}
