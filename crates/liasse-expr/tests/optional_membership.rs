#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! §optional-membership: an `optional<T>` needle against a `set<T>` (or a view).
//!
//! Since an `$optional` `$ref` field now types as `optional<ref<..>>` (symmetric
//! with optional scalars), a membership test such as a G5 aux-role check
//! `.role in v.roles` puts an `optional<T>` needle on the left of `in` while the
//! set element stays a bare `T`. `check_in` peels the leading `optional` for the
//! membership TYPE-check; at RUNTIME an absent (`none`) needle is simply not a
//! member (it never equals a present element), a present needle is the ordinary
//! membership test, and a bare (required) needle is unchanged.

mod common;

use std::collections::BTreeSet;

use common::{check_rejects, eval, keyless_row, row_type, scalar, scell, vint, vtext, FixedEnv, FixedScope};
use liasse_expr::{Cell, ExprType};
use liasse_value::{Ref, RefTarget, Type, Value};

/// Set of two text members `{admin, editor}`.
fn text_set() -> Value {
    Value::Set([vtext("admin"), vtext("editor")].into_iter().collect())
}

/// Set of two `ref<people>` members keyed `admin`/`editor`.
fn ref_set() -> Value {
    let members: BTreeSet<Value> = [
        Value::Ref(Ref::scalar(vtext("admin"))),
        Value::Ref(Ref::scalar(vtext("editor"))),
    ]
    .into_iter()
    .collect();
    Value::Set(members)
}

fn ref_ty() -> Type {
    Type::Ref(RefTarget::Scalar(Box::new(Type::Text)))
}

fn opt(inner: Type) -> ExprType {
    scalar(Type::Optional(Box::new(inner)))
}

fn set_of(inner: Type) -> ExprType {
    scalar(Type::Set(Box::new(inner)))
}

/// A scope + env whose root row carries every needle/haystack field the tests
/// probe, so each membership expression states its own state tree.
fn fixture() -> (FixedScope, FixedEnv, Cell) {
    let scope = FixedScope::new(ExprType::Row(row_type(
        vec![
            ("roles", set_of(Type::Text)),
            ("ref_roles", set_of(ref_ty())),
            ("int_roles", set_of(Type::Int)),
            ("opt_member", opt(Type::Text)),
            ("opt_nonmember", opt(Type::Text)),
            ("opt_absent", opt(Type::Text)),
            ("bare_member", scalar(Type::Text)),
            ("opt_ref_member", opt(ref_ty())),
            ("opt_ref_nonmember", opt(ref_ty())),
            ("opt_ref_absent", opt(ref_ty())),
            ("bare_ref_member", scalar(ref_ty())),
        ],
        None,
    )));

    let root = keyless_row(
        0,
        vec![
            ("roles", scell(text_set())),
            ("ref_roles", scell(ref_set())),
            ("int_roles", scell(Value::Set([vint(1), vint(2)].into_iter().collect()))),
            ("opt_member", scell(vtext("admin"))),
            ("opt_nonmember", scell(vtext("ghost"))),
            ("opt_absent", scell(Value::None)),
            ("bare_member", scell(vtext("admin"))),
            ("opt_ref_member", scell(Value::Ref(Ref::scalar(vtext("admin"))))),
            ("opt_ref_nonmember", scell(Value::Ref(Ref::scalar(vtext("ghost"))))),
            ("opt_ref_absent", scell(Value::None)),
            ("bare_ref_member", scell(Value::Ref(Ref::scalar(vtext("admin"))))),
        ],
    );
    let dot = Cell::Row(Box::new(root.clone()));
    (scope, FixedEnv::new(root), dot)
}

fn boolean(cell: Cell) -> Option<Value> {
    cell.as_scalar().cloned()
}

#[test]
fn optional_text_needle_in_text_set() {
    // Test 1: `optional<text> in set<text>` type-checks AND evaluates — present
    // member → true, present non-member → false, absent → false.
    let (scope, env, dot) = fixture();
    assert_eq!(
        boolean(eval(&scope, &env, &dot, ".opt_member in .roles")),
        Some(Value::Bool(true)),
        "a present optional needle that is a member is in the set",
    );
    assert_eq!(
        boolean(eval(&scope, &env, &dot, ".opt_nonmember in .roles")),
        Some(Value::Bool(false)),
        "a present optional needle that is not a member is not in the set",
    );
    assert_eq!(
        boolean(eval(&scope, &env, &dot, ".opt_absent in .roles")),
        Some(Value::Bool(false)),
        "an absent (none) needle is not a member",
    );
}

#[test]
fn optional_ref_needle_in_ref_set() {
    // Test 2: `optional<ref> in set<ref>` type-checks AND evaluates the same way.
    let (scope, env, dot) = fixture();
    assert_eq!(
        boolean(eval(&scope, &env, &dot, ".opt_ref_member in .ref_roles")),
        Some(Value::Bool(true)),
        "a present optional ref needle that is a member is in the set",
    );
    assert_eq!(
        boolean(eval(&scope, &env, &dot, ".opt_ref_nonmember in .ref_roles")),
        Some(Value::Bool(false)),
        "a present optional ref needle that is not a member is not in the set",
    );
    assert_eq!(
        boolean(eval(&scope, &env, &dot, ".opt_ref_absent in .ref_roles")),
        Some(Value::Bool(false)),
        "an absent (none) ref needle is not a member",
    );
}

#[test]
fn bare_needle_membership_is_unchanged() {
    // Test 3 (control): a bare (required) `text`/`ref` needle is unaffected.
    let (scope, env, dot) = fixture();
    assert_eq!(
        boolean(eval(&scope, &env, &dot, ".bare_member in .roles")),
        Some(Value::Bool(true)),
        "a bare text needle that is a member is still in the set",
    );
    assert_eq!(
        boolean(eval(&scope, &env, &dot, ".bare_ref_member in .ref_roles")),
        Some(Value::Bool(true)),
        "a bare ref needle that is a member is still in the set",
    );
}

#[test]
fn wrong_needle_type_is_still_rejected() {
    // Test 4 (control): a genuinely wrong needle type is STILL rejected — peeling
    // the optional does not paper over a real mismatch.
    let (scope, ..) = fixture();
    check_rejects(&scope, ".bare_member in .int_roles");
    // ... and an optional needle of the wrong payload type is rejected too.
    check_rejects(&scope, ".opt_member in .int_roles");
}
