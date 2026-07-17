#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! View evaluation over hand-built collections (§7). Expected rows and order
//! are deduced from §7 + Annex B, independent of the implementation.

mod common;

use common::{
    as_scalar, check, collection, eval, ids, keyed_row, keyless_row, row, row_type, rows_fields,
    scalar, scell, try_eval, vdec, vint, vtext, view, FixedEnv, FixedScope,
};
use liasse_expr::{Cell, EvalError, ExprType, Row};
use liasse_value::{Ref, RefTarget, Type, Value};

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
fn named_selector_binding_visible_in_projection() {
    // §6.4: a named selector `[:p]` binds each projected row to `p`, so the
    // projection body reads `p.field` — the binding must be in scope where the
    // outputs are typed and evaluated, not just inside a `[:p | …]` filter.
    let ty = people_type(vec![("name", scalar(Type::Text))]);
    let rows = vec![
        krow(1, "a", vec![("name", scell(vtext("Ann")))]),
        krow(2, "b", vec![("name", scell(vtext("Bo")))]),
    ];
    let (scope, env, dot) = one_collection("people", ty, rows);
    let result = eval(&scope, &env, &dot, ".people[:p] { who: p.id, name: p.name }");
    assert_eq!(
        rows_fields(&result),
        vec![
            vec![("name".to_owned(), vtext("Ann")), ("who".to_owned(), vtext("a"))],
            vec![("name".to_owned(), vtext("Bo")), ("who".to_owned(), vtext("b"))],
        ]
    );
}

#[test]
fn nested_object_output_projects_source_struct() {
    // §7.1 projection grammar `nested: { ... }`: a bare object output projects
    // the same-named source struct field. `.company { name, address: { city } }`
    // yields a nested `address` object exposing only `city`.
    let address_ty = row_type(
        vec![("city", scalar(Type::Text)), ("country", scalar(Type::Text))],
        None,
    );
    let company_ty = row_type(
        vec![("name", scalar(Type::Text)), ("address", ExprType::Row(address_ty))],
        None,
    );
    let root_ty = row_type(vec![("company", ExprType::Row(company_ty))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));

    let address_row = common::keyless_row(
        2,
        vec![("city", scell(vtext("Paris"))), ("country", scell(vtext("FR")))],
    );
    let company_row = common::keyless_row(
        1,
        vec![
            ("name", scell(vtext("Acme"))),
            ("address", Cell::Row(Box::new(address_row))),
        ],
    );
    let root = common::keyless_row(0, vec![("company", Cell::Row(Box::new(company_row)))]);
    let dot = Cell::Row(Box::new(root.clone()));
    let env = FixedEnv::new(root);

    let result = eval(&scope, &env, &dot, ".company { name, address: { city } }");
    let company = result.as_row().expect("a single company row");
    assert_eq!(as_scalar(company.cell("name").expect("name")), vtext("Acme"));
    let address = company.cell("address").expect("address").as_row().expect("nested struct");
    assert_eq!(as_scalar(address.cell("city").expect("city")), vtext("Paris"));
    // `country` was not projected, so it is absent from the nested object.
    assert!(address.cell("country").is_none(), "only projected members appear");
}

#[test]
fn field_access_through_view_flattens_nested_collection_and_keeps_outer_bind() {
    // §6.4: `.companies[:c].offices[:o]` reads the nested `offices` collection of
    // each company. Dotted field access on a view flattens exactly like `::`,
    // and both the outer bind `c` and the inner bind `o` stay visible in the
    // projection body. The same local office key `hq` under two parents yields
    // two distinct rows.
    let office_ty = row_type(
        vec![("id", scalar(Type::Text)), ("name", scalar(Type::Text))],
        Some(scalar(Type::Text)),
    );
    let company_ty = row_type(
        vec![("id", scalar(Type::Text)), ("offices", view(office_ty))],
        Some(scalar(Type::Text)),
    );
    let root_ty = row_type(vec![("companies", view(company_ty))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));

    let office = |key: &str, name: &str| {
        keyed_row(key, vtext(key), vec![("id", scell(vtext(key))), ("name", scell(vtext(name)))])
    };
    let acme = keyed_row(
        "acme",
        vtext("acme"),
        vec![("id", scell(vtext("acme"))), ("offices", collection(vec![office("hq", "Acme HQ")]))],
    );
    let globex = keyed_row(
        "globex",
        vtext("globex"),
        vec![("id", scell(vtext("globex"))), ("offices", collection(vec![office("hq", "Globex HQ")]))],
    );
    let root = keyless_row(0, vec![("companies", collection(vec![acme, globex]))]);
    let dot = Cell::Row(Box::new(root.clone()));
    let env = FixedEnv::new(root);

    let dotted = eval(
        &scope,
        &env,
        &dot,
        ".companies[:c].offices[:o] { company: c.id, office: o.id, name: o.name }",
    );
    assert_eq!(
        rows_fields(&dotted),
        vec![
            vec![
                ("company".to_owned(), vtext("acme")),
                ("name".to_owned(), vtext("Acme HQ")),
                ("office".to_owned(), vtext("hq")),
            ],
            vec![
                ("company".to_owned(), vtext("globex")),
                ("name".to_owned(), vtext("Globex HQ")),
                ("office".to_owned(), vtext("hq")),
            ],
        ]
    );

    // §6.4: the dotted spelling denotes the same traversal as `::`, which
    // auto-binds each traversed level to its field name.
    let colons = eval(
        &scope,
        &env,
        &dot,
        ".companies::offices { company: companies.id, office: offices.id, name: offices.name }",
    );
    assert_eq!(rows_fields(&colons), rows_fields(&dotted));
}

#[test]
fn structured_sort_form_matches_string_form() {
    // §7.3: the structured `{ $by, $dir }` entry expresses the same order as the
    // string form (`-n` / `n`). Both spellings must produce identical row order.
    let ty = people_type(vec![("n", scalar(Type::Int))]);
    let rows = vec![
        krow(1, "a", vec![("n", scell(vint(10)))]),
        krow(2, "b", vec![("n", scell(vint(30)))]),
        krow(3, "c", vec![("n", scell(vint(20)))]),
    ];
    let (scope, env, dot) = one_collection("items", ty, rows);

    let desc_string = eval(&scope, &env, &dot, ".items { id, n, $sort: [-n] }");
    let desc_struct =
        eval(&scope, &env, &dot, ".items { id, n, $sort: [ { $by: n, $dir: desc } ] }");
    assert_eq!(ids(&desc_string, "id"), vec![vtext("b"), vtext("c"), vtext("a")]);
    assert_eq!(ids(&desc_struct, "id"), ids(&desc_string, "id"));

    let asc_string = eval(&scope, &env, &dot, ".items { id, n, $sort: [n] }");
    let asc_struct = eval(&scope, &env, &dot, ".items { id, n, $sort: [ { $by: n, $dir: asc } ] }");
    assert_eq!(ids(&asc_string, "id"), vec![vtext("a"), vtext("c"), vtext("b")]);
    assert_eq!(ids(&asc_struct, "id"), ids(&asc_string, "id"));
}

#[test]
fn sort_string_form_denotes_column_expression() {
    // §7.3 / Annex B canonical wire form: every `$sort` entry is a *string*
    // holding the key expression (`["name", "id"]`, `["-created_at", "id"]`,
    // `["string.casefold(name)", "name"]`), optionally prefixed by `-` for
    // descending. The string is the column expression, not a text constant, so
    // it must reorder exactly as the compact bare form does. Externally derived:
    // n = {a:10, b:30, c:20}, so ascending is a,c,b and descending is b,c,a.
    let ty = people_type(vec![("n", scalar(Type::Int))]);
    let rows = vec![
        krow(1, "a", vec![("n", scell(vint(10)))]),
        krow(2, "b", vec![("n", scell(vint(30)))]),
        krow(3, "c", vec![("n", scell(vint(20)))]),
    ];
    let (scope, env, dot) = one_collection("items", ty, rows);

    let asc = eval(&scope, &env, &dot, ".items { id, n, $sort: [\"n\"] }");
    assert_eq!(ids(&asc, "id"), vec![vtext("a"), vtext("c"), vtext("b")]);

    let desc = eval(&scope, &env, &dot, ".items { id, n, $sort: [\"-n\"] }");
    assert_eq!(ids(&desc, "id"), vec![vtext("b"), vtext("c"), vtext("a")]);

    // The string carries a full expression, not just a bare name: `0 - n`
    // ascends by the negated value, which is descending by n.
    let expr = eval(&scope, &env, &dot, ".items { id, n, $sort: [\"0 - n\"] }");
    assert_eq!(ids(&expr, "id"), vec![vtext("b"), vtext("c"), vtext("a")]);

    // A string `$by` in the structured form is likewise the key expression.
    let by = eval(&scope, &env, &dot, ".items { id, n, $sort: [ { $by: \"n\", $dir: desc } ] }");
    assert_eq!(ids(&by, "id"), vec![vtext("b"), vtext("c"), vtext("a")]);
}

#[test]
fn sort_string_form_places_none_by_direction() {
    // §7.3 / B.2 through the canonical string spelling: present-then-none
    // ascending, none-then-present descending. Externally derived from the
    // absence-placement rule, independent of the bare-form path.
    let ty = people_type(vec![("score", scalar(Type::Optional(Box::new(Type::Int))))]);
    let rows = vec![
        krow(1, "s1", vec![("score", scell(vint(2)))]),
        krow(2, "s2", vec![("score", scell(Value::None))]),
        krow(3, "s3", vec![("score", scell(vint(1)))]),
    ];
    let (scope, env, dot) = one_collection("items", ty, rows);

    let asc = eval(&scope, &env, &dot, ".items { id, score, $sort: [\"score\"] }");
    assert_eq!(ids(&asc, "id"), vec![vtext("s3"), vtext("s1"), vtext("s2")]);

    let desc = eval(&scope, &env, &dot, ".items { id, score, $sort: [\"-score\"] }");
    assert_eq!(ids(&desc, "id"), vec![vtext("s2"), vtext("s1"), vtext("s3")]);
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

// §6.3 lines 700–702 make a projection's cardinality context-sensitive: a
// projection whose base is a lone scalar/composite key is a one-row `Row` (a
// value context — a `return`, a scalar row value, a mutation receiver —
// consumes it as a single object and rejects zero or several occurrences),
// while a projection over a filter, multi-key, or set selection is a multi-row
// `View`. The distinction is carried by the base selector's own type; the view
// counterpart is reached through `evaluate_view` (§12.2, the `$view` shape).
// These tests lock both directions.

#[test]
fn scalar_key_projection_is_a_row_in_a_value_context() {
    // §6.3 line 700: a lone scalar key contributes one row when present. In a
    // value context that is delivered as the single `Row` (a JSON object), not a
    // one-element collection.
    let ty = people_type(vec![("name", scalar(Type::Text))]);
    let rows = vec![
        krow(1, "a", vec![("name", scell(vtext("Ann")))]),
        krow(2, "b", vec![("name", scell(vtext("Bo")))]),
    ];
    let (scope, env, dot) = one_collection("people", ty, rows);
    let source = r#".people['a'] { id, name }"#;
    assert!(matches!(check(&scope, source).ty(), ExprType::Row(_)));
    let result = eval(&scope, &env, &dot, source);
    let Cell::Row(row) = &result else {
        panic!("a scalar-key projection in a value context must be one row, got {result:?}");
    };
    assert_eq!(row.cell("id"), Some(&scell(vtext("a"))));
    assert_eq!(row.cell("name"), Some(&scell(vtext("Ann"))));
}

#[test]
fn scalar_key_projection_absent_key_rejects_in_a_value_context() {
    // §6.3 line 702: a value context requires exactly one occurrence; an absent
    // scalar key is zero occurrences and rejects the whole evaluation.
    let ty = people_type(vec![("name", scalar(Type::Text))]);
    let rows = vec![krow(1, "a", vec![("name", scell(vtext("Ann")))])];
    let (scope, env, dot) = one_collection("people", ty, rows);
    let result = try_eval(&scope, &env, &dot, r#".people['zz'] { id, name }"#);
    assert!(
        matches!(result, Err(EvalError::Cardinality { found: 0, .. })),
        "an absent scalar key must reject, got {result:?}"
    );
}

#[test]
fn filter_projection_is_a_view() {
    // §6.3: a filter selection is a multi-row view, so its projection is a `View`
    // delivered as a collection regardless of context.
    let ty = people_type(vec![("grp", scalar(Type::Text))]);
    let rows = vec![
        krow(1, "a", vec![("grp", scell(vtext("L")))]),
        krow(2, "b", vec![("grp", scell(vtext("R")))]),
        krow(3, "c", vec![("grp", scell(vtext("L")))]),
    ];
    let (scope, env, dot) = one_collection("people", ty, rows);
    let source = r#".people[:p | p.grp == "L"] { id }"#;
    assert!(matches!(check(&scope, source).ty(), ExprType::View(_)));
    let result = eval(&scope, &env, &dot, source);
    assert!(matches!(result, Cell::Collection(_)));
    assert_eq!(ids(&result, "id"), vec![vtext("a"), vtext("c")]);
}

#[test]
fn multi_key_projection_is_a_view() {
    // §6.3: comma-separated key operands are concatenated into a multi-row view,
    // so the projection is a `View` (an array), even for present keys.
    let ty = people_type(vec![("name", scalar(Type::Text))]);
    let rows = vec![
        krow(1, "a", vec![("name", scell(vtext("Ann")))]),
        krow(2, "b", vec![("name", scell(vtext("Bo")))]),
    ];
    let (scope, env, dot) = one_collection("people", ty, rows);
    let source = r#".people['a', 'b'] { id }"#;
    assert!(matches!(check(&scope, source).ty(), ExprType::View(_)));
    let result = eval(&scope, &env, &dot, source);
    assert_eq!(ids(&result, "id"), vec![vtext("a"), vtext("b")]);
}

#[test]
fn scalar_key_projection_in_view_context_is_a_collection() {
    // §12.2: the same lone-scalar-key projection, delivered in view context (a
    // `$view`), is the 0/1-row view it denotes — one row present, none absent —
    // never a coerced single row and never a cardinality rejection.
    let ty = people_type(vec![("name", scalar(Type::Text))]);
    let rows = vec![krow(1, "a", vec![("name", scell(vtext("Ann")))])];
    let (scope, env, dot) = one_collection("people", ty, rows);

    let present = check(&scope, r#".people['a'] { id, name }"#)
        .evaluate_view(&env, &dot)
        .expect("view-context evaluation must not reject a present key");
    assert_eq!(
        rows_fields(&present),
        vec![vec![("id".to_owned(), vtext("a")), ("name".to_owned(), vtext("Ann"))]]
    );

    let absent = check(&scope, r#".people['zz'] { id, name }"#)
        .evaluate_view(&env, &dot)
        .expect("view-context evaluation must not reject an absent key");
    assert_eq!(absent, Cell::Collection(Vec::new()));
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
fn set_of_refs_selector_matches_targets_by_key_in_canonical_order() {
    // §6.3: "a set contributes keys in the target collection's canonical order."
    // §5.6: a ref's application-visible value is its target's current typed key,
    // so a set of refs seeded out of order ({c, a}) selects rows a then c — the
    // ref operands compare against the row key by that inner key, not by their
    // wrapping ref identity.
    use std::collections::BTreeSet;
    let acct_row = row_type(vec![("id", scalar(Type::Text))], Some(scalar(Type::Text)));
    let ref_ty = Type::Ref(RefTarget::Scalar(Box::new(Type::Text)));
    let root_ty = row_type(
        vec![
            ("accounts", view(acct_row)),
            ("reviewers", scalar(Type::Set(Box::new(ref_ty)))),
        ],
        None,
    );
    let scope = FixedScope::new(ExprType::Row(root_ty));
    let mut members = BTreeSet::new();
    members.insert(Value::Ref(Ref::scalar(vtext("c"))));
    members.insert(Value::Ref(Ref::scalar(vtext("a"))));
    let root = keyless_row(
        0,
        vec![
            (
                "accounts",
                collection(vec![
                    krow(1, "a", vec![]),
                    krow(2, "b", vec![]),
                    krow(3, "c", vec![]),
                ]),
            ),
            ("reviewers", scell(Value::Set(members))),
        ],
    );
    let dot = Cell::Row(Box::new(root.clone()));
    let env = FixedEnv::new(root);
    let result = eval(&scope, &env, &dot, "/accounts[/reviewers] { id }");
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
