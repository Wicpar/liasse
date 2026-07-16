#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! View row identity (§7.2, §12.4, Annex B.5, Annex D.1/D.2).
//!
//! A view row's identity derives from its key, never its materialized position,
//! so it survives the disappearance of earlier rows — the property §12.4's view
//! delta depends on. Expected identities are the Annex D.2 canonical key text of
//! each row's key, deduced from the spec, not from the implementation.

mod common;

use common::{
    as_scalar, check_rejects, collection, eval, keyed_row, keyless_row, row, row_ids, row_type,
    scalar, scell, vint, vtext, view, FixedEnv, FixedScope,
};
use liasse_expr::{Cell, ExprType, Row, RowId, RowIdPart};
use liasse_value::{Ref, RefTarget, Type, Value};

fn scope_over(name: &str, row_ty: liasse_expr::RowType, rows: Vec<Row>) -> (FixedScope, FixedEnv, Cell) {
    let root_ty = row_type(vec![(name, view(row_ty))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));
    let root = keyless_row(0, vec![(name, collection(rows))]);
    let dot = Cell::Row(Box::new(root.clone()));
    (scope, FixedEnv::new(root), dot)
}

fn entry_ty(extra: Vec<(&str, ExprType)>) -> liasse_expr::RowType {
    let mut fields = vec![("account", scalar(Type::Text)), ("debit", scalar(Type::Int))];
    fields.extend(extra);
    row_type(fields, Some(scalar(Type::Text)))
}

fn entry(seed: u64, key: &str, account: &str, debit: i64) -> Row {
    row(
        seed,
        vtext(key),
        vec![("account", scell(vtext(account))), ("debit", scell(vint(debit)))],
    )
}

/// §7.2 / §12.4: a synthetic-`$key` group's identity is its group key (rendered
/// to canonical key text), not the group's ordinal, so removing an entire
/// earlier group leaves every surviving group's identity unchanged.
#[test]
fn grouped_identity_is_group_key_not_position() {
    let src = ".entries { $key: account, account, total: sum(group.debit) }";

    let (s1, e1, d1) = scope_over(
        "entries",
        entry_ty(vec![]),
        vec![entry(1, "l1", "a", 10), entry(2, "l2", "b", 5), entry(3, "l3", "c", 3)],
    );
    let full = eval(&s1, &e1, &d1, src);
    assert_eq!(
        row_ids(&full),
        vec![RowId::keyed("a"), RowId::keyed("b"), RowId::keyed("c")],
    );

    // Drop the whole earliest group `a`; `b` and `c` keep the same identity.
    let (s2, e2, d2) = scope_over(
        "entries",
        entry_ty(vec![]),
        vec![entry(2, "l2", "b", 5), entry(3, "l3", "c", 3)],
    );
    let fewer = eval(&s2, &e2, &d2, src);
    assert_eq!(row_ids(&fewer), vec![RowId::keyed("b"), RowId::keyed("c")]);
}

/// Annex D.2: a composite synthetic key joins its components with `:` in `$key`
/// order, and a `:` inside a component is escaped `%3A` before the join, so the
/// join separator is unambiguous.
#[test]
fn composite_group_identity_is_canonical_key_text() {
    let ty = row_type(
        vec![
            ("account", scalar(Type::Text)),
            ("kind", scalar(Type::Text)),
            ("debit", scalar(Type::Int)),
        ],
        Some(scalar(Type::Text)),
    );
    let rows = vec![row(
        1,
        vtext("l1"),
        vec![
            ("account", scell(vtext("a:b"))),
            ("kind", scell(vtext("x"))),
            ("debit", scell(vint(4))),
        ],
    )];
    let (scope, env, dot) = scope_over("entries", ty, rows);
    let src = ".entries { $key: [account, kind], account, kind, total: sum(group.debit) }";
    let result = eval(&scope, &env, &dot, src);
    // account "a:b" escapes to "a%3Ab", joined with kind "x" by ":".
    assert_eq!(row_ids(&result), vec![RowId::keyed("a%3Ab:x")]);
}

/// §6.3: "Equality between a row or ref and a key of the same declared target
/// compares the current typed key." A `ref<text-target>` is comparable with a
/// `text` key, and the comparison reads the ref's current typed key — equal
/// when the key matches, unequal otherwise.
#[test]
fn ref_compares_with_current_typed_key() {
    let ref_ty = scalar(Type::Ref(RefTarget::Scalar(Box::new(Type::Text))));
    let member_ty = row_type(vec![("team", ref_ty)], Some(scalar(Type::Text)));
    let scope = FixedScope::new(ExprType::Row(member_ty));

    let member = row(
        1,
        vtext("m1"),
        vec![("team", scell(Value::Ref(Ref::scalar(vtext("good")))))],
    );
    let env = FixedEnv::new(member.clone());
    let dot = Cell::Row(Box::new(member));

    assert_eq!(as_scalar(&eval(&scope, &env, &dot, ".team == 'good'")), Value::Bool(true));
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, ".team == 'banned'")), Value::Bool(false));
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, ".team != 'banned'")), Value::Bool(true));
}

/// §6.3: a key of a *different* type than the ref's declared target key is not
/// "a key of the same declared target", so the comparison is statically
/// rejected rather than silently always-unequal.
#[test]
fn ref_comparison_with_foreign_key_type_is_rejected() {
    let ref_ty = scalar(Type::Ref(RefTarget::Scalar(Box::new(Type::Text))));
    let member_ty = row_type(vec![("team", ref_ty)], Some(scalar(Type::Text)));
    let scope = FixedScope::new(ExprType::Row(member_ty));
    // `5` is an `int`; the target key is `text`, so the two are incomparable.
    assert!(check_rejects(&scope, ".team == 5").has_errors());
}

/// §7.2: a plain projection preserves inherited identity — each output row keeps
/// its source row's key-derived identity.
#[test]
fn plain_projection_inherits_source_identity() {
    let rows = vec![
        keyed_row("a", vtext("a"), vec![("account", scell(vtext("a"))), ("debit", scell(vint(1)))]),
        keyed_row("b", vtext("b"), vec![("account", scell(vtext("b"))), ("debit", scell(vint(2)))]),
    ];
    let (scope, env, dot) = scope_over("entries", entry_ty(vec![]), rows);
    let result = eval(&scope, &env, &dot, ".entries { account }");
    assert_eq!(row_ids(&result), vec![RowId::keyed("a"), RowId::keyed("b")]);
}

/// Annex B.5: a keyless projection has no key identity, so it falls back to
/// occurrence order — an `Occurrence` component, never a `Key` one.
#[test]
fn keyless_projection_falls_back_to_occurrence() {
    let ty = row_type(vec![("account", scalar(Type::Text))], None);
    let rows = vec![
        keyless_row(7, vec![("account", scell(vtext("a")))]),
        keyless_row(9, vec![("account", scell(vtext("b")))]),
    ];
    let (scope, env, dot) = scope_over("entries", ty, rows);
    let result = eval(&scope, &env, &dot, ".entries { account }");
    let ids = row_ids(&result);
    assert!(ids.iter().all(|id| matches!(id.parts(), [RowIdPart::Occurrence(_)])));
    assert_eq!(ids, vec![RowId::leaf(7), RowId::leaf(9)]);
}
