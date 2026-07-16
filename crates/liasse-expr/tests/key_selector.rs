#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! The `.$key` selector (§6.2/§6.3): the identity key value of a bound keyed
//! row. Expected values are the row keys the harness seeds, deduced from the
//! spec rule that a row's `.$key` is the same value a key selector matches
//! against — independent of the implementation.

mod common;

use common::{
    as_scalar, check_rejects, eval, keyed_row, row_type, scalar, scell, vtext, view, FixedEnv,
    FixedScope,
};
use liasse_expr::{Cell, ExprType, Row, RowType};
use liasse_value::{Struct, StructType, Text, Type, Value};

/// A keyed row whose `key` doubles as an `id` cell.
fn krow(key: &str) -> Row {
    keyed_row(key, vtext(key), vec![("id", scell(vtext(key)))])
}

/// A person row type: `id: text`, keyed by `text`.
fn people_type() -> RowType {
    row_type(vec![("id", scalar(Type::Text))], Some(scalar(Type::Text)))
}

/// A scope/env exposing a keyed collection `people` and a structural `$actor`
/// bound to one keyed row.
fn actor_scope(actor: Row) -> (FixedScope, FixedEnv, Cell) {
    let root_ty = row_type(vec![("people", view(people_type()))], None);
    let mut scope = FixedScope::new(ExprType::Row(root_ty));
    scope
        .structurals
        .insert("actor".to_owned(), ExprType::Row(people_type()));
    let root = common::keyless_row(
        0,
        vec![(
            "people",
            Cell::Collection(vec![krow("alice"), krow("bob")]),
        )],
    );
    let dot = Cell::Row(Box::new(root.clone()));
    let env = FixedEnv::new(root).structural("actor", Cell::Row(Box::new(actor)));
    (scope, env, dot)
}

#[test]
fn key_selector_on_keyed_selection_reads_the_row_key() {
    // §6.3: `.people['bob'].$key` is bob's identity key. A scalar-key selection
    // is one row, and its `.$key` is exactly the key that selected it.
    let (scope, env, dot) = actor_scope(krow("alice"));
    let result = eval(&scope, &env, &dot, ".people['bob'].$key");
    assert_eq!(as_scalar(&result), vtext("bob"));
}

#[test]
fn key_selector_on_structural_actor_reads_its_key() {
    // §6.2: `$actor` is a bound keyed row; `$actor.$key` is that row's key.
    // Bound to alice, so the selector yields "alice".
    let (scope, env, dot) = actor_scope(krow("alice"));
    let result = eval(&scope, &env, &dot, "$actor.$key");
    assert_eq!(as_scalar(&result), vtext("alice"));
}

#[test]
fn key_selector_feeds_a_key_lookup() {
    // §6.3: `.people[$actor.$key]` selects the row whose key is the actor's key.
    // A lone scalar-key selection is one row (a value context), and the actor is
    // alice, so the lookup returns alice's row (id == "alice").
    let (scope, env, dot) = actor_scope(krow("alice"));
    let result = eval(&scope, &env, &dot, ".people[$actor.$key] { id }");
    let Cell::Row(row) = &result else {
        panic!("a scalar-key selection is one row, got {result:?}");
    };
    assert_eq!(row.cell("id").and_then(Cell::as_scalar), Some(&vtext("alice")));
}

#[test]
fn key_selector_on_composite_key_reads_the_key_struct() {
    // A composite `$key` (§5.4) yields the key as a struct of its components;
    // `.$key` returns that whole struct value.
    let key_ty = Type::Struct(StructType::new([
        ("code".to_owned(), Type::Text),
        ("country".to_owned(), Type::Text),
    ]));
    let key_struct = Value::Struct(Struct::new([
        (Text::new("code"), vtext("VAT")),
        (Text::new("country"), vtext("FR")),
    ]));
    let rate_ty = row_type(
        vec![
            ("country", scalar(Type::Text)),
            ("code", scalar(Type::Text)),
        ],
        Some(scalar(key_ty)),
    );
    let rate = keyed_row(
        "{code:VAT,country:FR}",
        key_struct.clone(),
        vec![
            ("country", scell(vtext("FR"))),
            ("code", scell(vtext("VAT"))),
        ],
    );
    let mut scope = FixedScope::new(ExprType::Row(row_type(vec![], None)));
    scope
        .structurals
        .insert("rate".to_owned(), ExprType::Row(rate_ty));
    let root = common::keyless_row(0, vec![]);
    let dot = Cell::Row(Box::new(root.clone()));
    let env = FixedEnv::new(root).structural("rate", Cell::Row(Box::new(rate)));
    let result = eval(&scope, &env, &dot, "$rate.$key");
    assert_eq!(as_scalar(&result), key_struct);
}

#[test]
fn key_selector_rejects_a_keyless_row() {
    // A keyless static struct has no identity key, so `.$key` is a static error
    // (§6.3: the selector needs a keyed row).
    let struct_ty = row_type(vec![("id", scalar(Type::Text))], None);
    let mut scope = FixedScope::new(ExprType::Row(row_type(vec![], None)));
    scope
        .structurals
        .insert("s".to_owned(), ExprType::Row(struct_ty));
    check_rejects(&scope, "$s.$key");
}

#[test]
fn key_selector_rejects_a_scalar_base() {
    // `.$key` reads a row's identity; a scalar has none, so it rejects.
    let mut scope = FixedScope::new(ExprType::Row(row_type(vec![], None)));
    scope
        .structurals
        .insert("name".to_owned(), ExprType::scalar(Type::Text));
    check_rejects(&scope, "$name.$key");
}
