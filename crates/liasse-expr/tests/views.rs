#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! View evaluation over hand-built collections (§7). Expected rows and order
//! are deduced from §7 + Annex B, independent of the implementation.

mod common;

use common::{
    as_scalar, collection, eval, ids, row, row_type, rows_fields, scalar, scell, vdec, vint,
    vtext, view, FixedEnv, FixedScope,
};
use liasse_expr::{Cell, ExprType, Row};
use liasse_value::{Type, Value};

/// Build a keyed row whose `key` doubles as an `id` cell, plus extra cells.
fn krow(seed: u64, key: &str, cells: Vec<(&str, Cell)>) -> Row {
    let mut all = vec![("id", scell(vtext(key)))];
    all.extend(cells);
    row(seed, vtext(key), all)
}

/// A scope/env whose root exposes one keyed collection `name`.
fn one_collection(
    name: &str,
    row_ty: liasse_expr::RowType,
    rows: Vec<Row>,
) -> (FixedScope, FixedEnv, Cell) {
    let root_ty = row_type(vec![(name, view(row_ty))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));
    let root = common::keyless_row(0, vec![(name, collection(rows))]);
    let dot = Cell::Row(Box::new(root.clone()));
    (scope, FixedEnv::new(root), dot)
}

fn people_type(extra: Vec<(&str, ExprType)>) -> liasse_expr::RowType {
    let mut fields = vec![("id", scalar(Type::Text))];
    fields.extend(extra);
    row_type(fields, Some(scalar(Type::Text)))
}

#[test]
fn projection_limits_visible_fields() {
    let ty = people_type(vec![("name", scalar(Type::Text))]);
    let rows = vec![
        krow(1, "a", vec![("name", scell(vtext("Ann")))]),
        krow(2, "b", vec![("name", scell(vtext("Bo")))]),
    ];
    let (scope, env, dot) = one_collection("people", ty, rows);
    let result = eval(&scope, &env, &dot, ".people { id }");
    assert_eq!(
        rows_fields(&result),
        vec![
            vec![("id".to_owned(), vtext("a"))],
            vec![("id".to_owned(), vtext("b"))],
        ]
    );
}

#[test]
fn projection_members_cross_reference_in_dependency_order() {
    // §7.1: `shout` depends on `base` depends on source field `first`.
    let ty = people_type(vec![("first", scalar(Type::Text))]);
    let rows = vec![krow(1, "a", vec![("first", scell(vtext("Ada")))])];
    let (scope, env, dot) = one_collection("people", ty, rows);
    let result = eval(&scope, &env, &dot, r#".people { base: first, shout: base + "!" }"#);
    assert_eq!(
        rows_fields(&result),
        vec![vec![
            ("base".to_owned(), vtext("Ada")),
            ("shout".to_owned(), vtext("Ada!")),
        ]]
    );
}

#[test]
fn sort_ascending_places_none_last() {
    // §7.3 / B.2: present ascending, then none.
    let ty = people_type(vec![("score", scalar(Type::Optional(Box::new(Type::Int))))]);
    let rows = vec![
        krow(1, "s1", vec![("score", scell(vint(2)))]),
        krow(2, "s2", vec![("score", scell(Value::None))]),
        krow(3, "s3", vec![("score", scell(vint(1)))]),
    ];
    let (scope, env, dot) = one_collection("items", ty, rows);
    let result = eval(&scope, &env, &dot, ".items { id, score, $sort: [score] }");
    assert_eq!(ids(&result, "id"), vec![vtext("s3"), vtext("s1"), vtext("s2")]);
}

#[test]
fn sort_descending_prefix_reverses_and_none_first() {
    let ty = people_type(vec![("score", scalar(Type::Optional(Box::new(Type::Int))))]);
    let rows = vec![
        krow(1, "s1", vec![("score", scell(vint(2)))]),
        krow(2, "s2", vec![("score", scell(Value::None))]),
        krow(3, "s3", vec![("score", scell(vint(1)))]),
    ];
    let (scope, env, dot) = one_collection("items", ty, rows);
    let result = eval(&scope, &env, &dot, ".items { id, score, $sort: [-score] }");
    // Descending: none first, then present descending (2, 1).
    assert_eq!(ids(&result, "id"), vec![vtext("s2"), vtext("s1"), vtext("s3")]);
}

#[test]
fn skip_and_limit_select_ordered_window() {
    let ty = people_type(vec![]);
    let rows = (1..=5)
        .map(|n| krow(n, &format!("k{n}"), vec![]))
        .collect();
    let (scope, env, dot) = one_collection("items", ty, rows);
    let result = eval(&scope, &env, &dot, ".items { id, $skip: 1, $limit: 2 }");
    assert_eq!(ids(&result, "id"), vec![vtext("k2"), vtext("k3")]);
}

#[test]
fn comma_selector_operands_concatenate_in_order_with_repeats() {
    // §6.3: independent key sources concatenate; repeats stay distinct.
    let ty = people_type(vec![("name", scalar(Type::Text))]);
    let rows = vec![
        krow(1, "a", vec![("name", scell(vtext("Ann")))]),
        krow(2, "b", vec![("name", scell(vtext("Bo")))]),
    ];
    let (scope, env, dot) = one_collection("people", ty, rows);
    let result = eval(&scope, &env, &dot, r#".people['b', 'a', 'b'] { id }"#);
    assert_eq!(ids(&result, "id"), vec![vtext("b"), vtext("a"), vtext("b")]);
}

#[test]
fn scalar_key_selector_then_field_reads_one_row() {
    let ty = people_type(vec![("name", scalar(Type::Text))]);
    let rows = vec![krow(1, "a", vec![("name", scell(vtext("Ann")))])];
    let (scope, env, dot) = one_collection("people", ty, rows);
    let result = eval(&scope, &env, &dot, r#".people['a'].name"#);
    assert_eq!(as_scalar(&result), vtext("Ann"));
}

#[test]
fn filter_selects_matching_rows() {
    let ty = people_type(vec![("grp", scalar(Type::Text))]);
    let rows = vec![
        krow(1, "a", vec![("grp", scell(vtext("L")))]),
        krow(2, "b", vec![("grp", scell(vtext("R")))]),
        krow(3, "c", vec![("grp", scell(vtext("L")))]),
    ];
    let (scope, env, dot) = one_collection("people", ty, rows);
    let result = eval(&scope, &env, &dot, r#".people[:p | p.grp == "L"] { id }"#);
    assert_eq!(ids(&result, "id"), vec![vtext("a"), vtext("c")]);
}

#[test]
fn union_concatenates_left_then_new_right_identities() {
    // §7.4: left order, then right identities not already present.
    let ty = people_type(vec![("grp", scalar(Type::Text))]);
    let rows = vec![
        krow(1, "a", vec![("grp", scell(vtext("L")))]),
        krow(2, "b", vec![("grp", scell(vtext("L")))]),
        krow(3, "c", vec![("grp", scell(vtext("R")))]),
        krow(4, "d", vec![("grp", scell(vtext("R")))]),
    ];
    let (scope, env, dot) = one_collection("people", ty, rows);
    let source = r#".people[:p | p.grp == "L"] { id } | .people[:p | p.grp == "R"] { id }"#;
    let result = eval(&scope, &env, &dot, source);
    assert_eq!(
        ids(&result, "id"),
        vec![vtext("a"), vtext("b"), vtext("c"), vtext("d")]
    );
}

#[test]
fn synthetic_key_groups_rows_with_aggregate() {
    // §7.2 / §7.5: rows sharing the synthetic key form one group; `total`
    // aggregates over `group`; groups appear in synthetic-key ascending order.
    let ty = row_type(
        vec![
            ("id", scalar(Type::Text)),
            ("account", scalar(Type::Text)),
            ("debit", scalar(Type::Int)),
        ],
        Some(scalar(Type::Text)),
    );
    let rows = vec![
        krow(1, "l1", vec![("account", scell(vtext("a"))), ("debit", scell(vint(10)))]),
        krow(2, "l2", vec![("account", scell(vtext("a"))), ("debit", scell(vint(5)))]),
        krow(3, "l3", vec![("account", scell(vtext("b"))), ("debit", scell(vint(3)))]),
    ];
    let (scope, env, dot) = one_collection("lines", ty, rows);
    let source = ".lines { $key: account, account, total: sum(group.debit) }";
    let result = eval(&scope, &env, &dot, source);
    assert_eq!(
        rows_fields(&result),
        vec![
            vec![("account".to_owned(), vtext("a")), ("total".to_owned(), vint(15))],
            vec![("account".to_owned(), vtext("b")), ("total".to_owned(), vint(3))],
        ]
    );
}

#[test]
fn aggregates_over_empty_input_have_spec_identities() {
    // §7.5: count -> 0, sum -> numeric zero, max -> none.
    let ty = row_type(
        vec![("id", scalar(Type::Text)), ("amount", scalar(Type::Int))],
        Some(scalar(Type::Text)),
    );
    let (scope, env, dot) = one_collection("items", ty, Vec::new());
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "count(.items)")), vint(0));
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "sum(.items.amount)")), vint(0));
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "max(.items.amount)")), Value::None);
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "avg(.items.amount)")), Value::None);
}

#[test]
fn avg_converts_to_decimal_and_divides() {
    // §7.5: avg over int { 1, 2 } is 3/2 = 1.5 as an exact decimal.
    let ty = row_type(
        vec![("id", scalar(Type::Text)), ("amount", scalar(Type::Int))],
        Some(scalar(Type::Text)),
    );
    let rows = vec![
        krow(1, "a", vec![("amount", scell(vint(1)))]),
        krow(2, "b", vec![("amount", scell(vint(2)))]),
    ];
    let (scope, env, dot) = one_collection("items", ty, rows);
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "avg(.items.amount)")), vdec("1.5"));
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "sum(.items.amount)")), vint(3));
}

#[test]
fn aggregate_skips_absent_inputs() {
    // §7.5: absent inputs are skipped; sum over {5, none, 3} is 8.
    let ty = row_type(
        vec![
            ("id", scalar(Type::Text)),
            ("amount", scalar(Type::Optional(Box::new(Type::Int)))),
        ],
        Some(scalar(Type::Text)),
    );
    let rows = vec![
        krow(1, "a", vec![("amount", scell(vint(5)))]),
        krow(2, "b", vec![("amount", scell(Value::None))]),
        krow(3, "c", vec![("amount", scell(vint(3)))]),
    ];
    let (scope, env, dot) = one_collection("items", ty, rows);
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "sum(.items.amount)")), vint(8));
    assert_eq!(as_scalar(&eval(&scope, &env, &dot, "min(.items.amount)")), vint(3));
}
