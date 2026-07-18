#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Red-team probe: composite-keyed ref dereference / membership / equality
//! (§6.3, §7.6, A.9).
//!
//! A ref to a collection with a composite `$key` is typed `RefTarget::Composite`
//! of the target's `$key`-order component types (`liasse-model` `refs::key_type`),
//! so a composite ref value is carried as `Ref::composite` — the positional
//! `$key`-order tuple. A composite target row materializes its key as the equal
//! positional `Value::Composite` (`materialize::key_identity`). Because the two
//! share one positional representation, dereference, `in`, and `==` compare the
//! ref's application key against the target row's key tuple directly. These tests
//! pin that the composite ref resolves to its target; expectations are derived
//! from §7.6 ("a ref value is a target key") and §6.3 (a ref and a key of the
//! same declared target compare the current typed key), not the impl.

mod common;

use common::{
    collection, eval, keyed_row, keyless_row, row_type, scalar, scell, vtext, view, FixedEnv,
    FixedScope,
};
use liasse_expr::{Cell, ExprType};
use liasse_value::{Ref, RefTarget, Type, Value};

/// The composite key `[region, code] = [eu, x]` as its application-visible
/// value: the positional `$key`-order tuple `Value::Composite([eu, x])`.
fn key_struct() -> Value {
    Value::Composite(vec![vtext("eu"), vtext("x")])
}

/// Root with `regions` (composite key `[region, code]`) and an `owner` ref field
/// carrying the composite key `owner`. `owner` is supplied by the caller so the
/// same fixture covers both faithful carriers of a composite ref value.
fn setup(owner: Value) -> (FixedScope, FixedEnv, Cell) {
    let key_ty = Type::Composite(vec![
        ("region".to_owned(), Type::Text),
        ("code".to_owned(), Type::Text),
    ]);
    let region_ty = row_type(
        vec![("region", scalar(Type::Text)), ("code", scalar(Type::Text)), ("label", scalar(Type::Text))],
        Some(scalar(key_ty.clone())),
    );
    // A ref to a composite-keyed target is typed RefTarget::Composite (positional).
    let owner_ty = scalar(Type::Ref(RefTarget::for_key(&key_ty)));
    let root_ty = row_type(vec![("regions", view(region_ty)), ("owner", owner_ty)], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));

    let region = keyed_row(
        "eu:x",
        key_struct(),
        vec![("region", scell(vtext("eu"))), ("code", scell(vtext("x"))), ("label", scell(vtext("EU-X")))],
    );
    let root = keyless_row(0, vec![("regions", collection(vec![region])), ("owner", scell(owner))]);
    let dot = Cell::Row(Box::new(root.clone()));
    (scope, FixedEnv::new(root), dot)
}

/// The two faithful carriers of a composite ref value: wrapped in `Ref`, or
/// stored as its bare positional key tuple (§6.3 ref/key equality permits either).
fn scalar_ref() -> Value {
    Value::Ref(Ref::composite(vec![vtext("eu"), vtext("x")]))
}
fn bare_key() -> Value {
    key_struct()
}

#[test]
fn control_object_selector_finds_the_composite_row() {
    // The target is genuinely present and reachable by its key: an object key
    // selector (A.9 authoring syntax for the same typed tuple) reads its field.
    let (scope, env, dot) = setup(scalar_ref());
    let label = eval(&scope, &env, &dot, ".regions[{ region: 'eu', code: 'x' }].label");
    assert_eq!(label.as_scalar(), Some(&vtext("EU-X")));
}

#[test]
fn composite_ref_membership_in_view() {
    // §7.6/§6.3: the ref's target key equals the region's identity key, so it is
    // a member of `.regions`.
    for owner in [scalar_ref(), bare_key()] {
        let (scope, env, dot) = setup(owner);
        let present = eval(&scope, &env, &dot, ".owner in .regions");
        assert_eq!(present.as_scalar(), Some(&Value::Bool(true)), "composite ref must be a member");
    }
}

#[test]
fn composite_ref_equality_with_key() {
    // §6.3: a ref equals a key of the same declared target when the typed keys
    // match.
    let (scope, env, dot) = setup(scalar_ref());
    let eq = eval(&scope, &env, &dot, ".owner == { region: 'eu', code: 'x' }");
    assert_eq!(eq.as_scalar(), Some(&Value::Bool(true)));
}

#[test]
fn composite_ref_deref_selects_exactly_one_row() {
    // §7.6: `.regions[.owner]` dereferences the ref to its one target row; a lone
    // composite-key selection is a one-row value context, so `.label` reads it.
    for owner in [scalar_ref(), bare_key()] {
        let (scope, env, dot) = setup(owner);
        let label = eval(&scope, &env, &dot, ".regions[.owner].label");
        assert_eq!(label.as_scalar(), Some(&vtext("EU-X")), "deref must select the one target row");
    }
}
