#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! The `$config` structural binding (SPEC.md §13.1): a module package's
//! immutable typed `$config` struct, read by the module's own expressions. The
//! declared struct is bound as the `$config` structural row, so `$config` reads
//! the whole struct and `$config.member` reads a member as an ordinary field of
//! that row. Expected results are deduced from §13.1 alone — the member the
//! schema declares has the schema's type, and its value is the bound install
//! value — independent of the implementation.

mod common;

use common::{
    as_scalar, check, check_rejects, eval, keyless_row, row_type, rowt, scalar, scell, vtext,
    FixedEnv, FixedScope,
};
use liasse_expr::{Cell, ExprType, RowType};
use liasse_value::Type;

/// A module `$config` struct schema: `currency: text`, `region: text` — a
/// keyless struct row (§13.1), exactly what the model resolves `$config` to.
fn config_schema() -> RowType {
    row_type(
        vec![("currency", scalar(Type::Text)), ("region", scalar(Type::Text))],
        None,
    )
}

/// A scope whose `$config` is bound to the declared schema, plus the matching
/// environment binding the install-supplied values.
fn config_scope() -> (FixedScope, FixedEnv, Cell) {
    let scope = FixedScope::new(rowt(row_type(vec![], None)))
        .structural("config", ExprType::Row(config_schema()));
    let root = keyless_row(0, vec![]);
    let dot = Cell::Row(Box::new(root.clone()));
    let config_value = keyless_row(
        1,
        vec![("currency", scell(vtext("EUR"))), ("region", scell(vtext("FR")))],
    );
    let env = FixedEnv::new(root).structural("config", Cell::Row(Box::new(config_value)));
    (scope, env, dot)
}

#[test]
fn config_member_read_types_against_the_declared_schema() {
    // §13.1: `$config` is the declared struct; `$config.currency` reads its
    // `currency` member, whose declared type is `text`.
    let (scope, _env, _dot) = config_scope();
    let typed = check(&scope, "$config.currency");
    assert_eq!(typed.ty().as_scalar(), Some(&Type::Text));
}

#[test]
fn bare_config_types_as_the_declared_struct_row() {
    // §13.1: a bare `$config` read is the whole immutable struct — the declared
    // struct row, not one member.
    let (scope, _env, _dot) = config_scope();
    let typed = check(&scope, "$config");
    assert_eq!(typed.ty().as_row(), Some(&config_schema()));
}

#[test]
fn unknown_config_member_is_rejected() {
    // §13.1: `$config` is a *closed* typed struct; reading a member it does not
    // declare is a static type error (the analogue of an install supplying an
    // undeclared member).
    let (scope, _env, _dot) = config_scope();
    let diags = check_rejects(&scope, "$config.tax_id");
    assert!(
        diags.iter().any(|d| d.message().contains("tax_id")),
        "the rejection names the undeclared member: {}",
        diags.iter().map(liasse_diag::Diagnostic::message).collect::<Vec<_>>().join("; ")
    );
}

#[test]
fn config_member_evaluates_to_the_bound_install_value() {
    // §13.1: `module expressions read it through $config`; with the install
    // supplying `currency: "EUR"`, `$config.currency` evaluates to that value.
    let (scope, env, dot) = config_scope();
    let result = eval(&scope, &env, &dot, "$config.currency");
    assert_eq!(as_scalar(&result), vtext("EUR"));
}

#[test]
fn config_member_read_flows_into_a_projection() {
    // §13.1 read-through in a projection (`.{ ..., currency: $config.currency }`),
    // the exact shape a module's exposed `$view` uses: the projected `currency`
    // takes the config value, distinct from the row's own fields.
    let scope = FixedScope::new(rowt(row_type(
        vec![("id", scalar(Type::Text)), ("label", scalar(Type::Text))],
        Some(scalar(Type::Text)),
    )))
    .structural("config", ExprType::Row(config_schema()));
    let typed = check(&scope, ". { id, label, currency: $config.currency }");
    let row = typed.ty().as_row().expect("a single-row projection");
    assert_eq!(row.field("currency").and_then(ExprType::as_scalar), Some(&Type::Text));

    let dot = Cell::Row(Box::new(keyless_row(
        0,
        vec![("id", scell(vtext("std"))), ("label", scell(vtext("Standard")))],
    )));
    let config_value = keyless_row(
        1,
        vec![("currency", scell(vtext("USD"))), ("region", scell(vtext("US")))],
    );
    let env = FixedEnv::new(keyless_row(0, vec![]))
        .structural("config", Cell::Row(Box::new(config_value)));
    let result = eval(&scope, &env, &dot, ". { id, label, currency: $config.currency }");
    let row = result.as_row().expect("a single projected row");
    assert_eq!(row.cell("currency").and_then(Cell::as_scalar), Some(&vtext("USD")));
}
