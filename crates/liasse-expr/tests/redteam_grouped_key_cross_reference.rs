#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! FINDING (§7.1/§7.2): a grouped view whose synthetic `$key` has a component
//! that reads ANOTHER `$key` component (an output-to-output cross reference the
//! §7.1 acyclic-reference rule permits) FAULTS at read with an unbound-name
//! error, instead of grouping the rows.
//!
//! §7.1 (verbatim): "Projection members are unordered named outputs. They MAY
//! refer to one another when their dependency graph is acyclic; the
//! implementation evaluates them in any valid dependency order."
//!
//! §7.2: "An array groups several output fields into one composite key, in the
//! listed order. ... Every non-key source value MUST be aggregated or derived
//! solely from key values." A key component is itself a key value, so a later
//! key component derived from an earlier one (`tag: acct + "-x"`, both keys) is
//! well-formed — it is "derived solely from key values". The checker accepts it
//! (`check/project.rs` orders outputs by their cross references and exempts key
//! outputs from the non-key-field check).
//!
//! But the grouped-key evaluator `eval/views.rs::group_key` binds ONLY the
//! `scope.binds` (the `::`/`[:name]` row bindings the wave-1 fix added) before
//! evaluating each `$key` output — it never binds the intermediate `$key`
//! outputs it has already computed. So when a `$key` component references another
//! `$key` output (lowered to a `LocalBinding`), the lookup is unbound and the
//! whole grouped read errors. `project_row`/`eval_keys` bind their computed
//! outputs, so only `group_key` is missing this.
//!
//! Root cause: `crates/liasse-expr/src/eval/views.rs::group_key` (~L335-364) —
//! the loop over `projection.key` evaluates each key output but never
//! `self.bind`s the computed value, unlike `project_row` (~L254-261).

mod common;

use common::{
    collection, eval, keyless_row, row, row_type, rows_fields, scalar, scell, try_eval, view,
    vint, vtext, FixedEnv, FixedScope,
};
use liasse_expr::{Cell, ExprType};
use liasse_value::Type;

/// A `lines` collection with an `account` text source field and a `debit` int.
fn lines_scope() -> (FixedScope, FixedEnv, Cell) {
    let ty = row_type(
        vec![
            ("id", scalar(Type::Text)),
            ("account", scalar(Type::Text)),
            ("debit", scalar(Type::Int)),
        ],
        Some(scalar(Type::Text)),
    );
    let rows = vec![
        row(1, vtext("l1"), vec![("id", scell(vtext("l1"))), ("account", scell(vtext("a"))), ("debit", scell(vint(10)))]),
        row(2, vtext("l2"), vec![("id", scell(vtext("l2"))), ("account", scell(vtext("a"))), ("debit", scell(vint(5)))]),
        row(3, vtext("l3"), vec![("id", scell(vtext("l3"))), ("account", scell(vtext("b"))), ("debit", scell(vint(3)))]),
    ];
    let root_ty = row_type(vec![("lines", view(ty))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));
    let root = keyless_row(0, vec![("lines", collection(rows))]);
    let dot = Cell::Row(Box::new(root.clone()));
    (scope, FixedEnv::new(root), dot)
}

/// The two groups §7.2 mandates for either spelling: accounts `a` and `b`, each
/// carrying `tag = account + "-x"`, in synthetic-key ascending order.
fn expected_groups() -> Vec<Vec<(String, liasse_value::Value)>> {
    vec![
        vec![("acct".to_owned(), vtext("a")), ("tag".to_owned(), vtext("a-x"))],
        vec![("acct".to_owned(), vtext("b")), ("tag".to_owned(), vtext("b-x"))],
    ]
}

/// CONTROL — a composite `$key` whose second component reads the SOURCE FIELD
/// directly (`tag: account + "-x"`) groups correctly. `group_key` can read the
/// source field off the pushed `.`, so this path never faults. This proves the
/// grouping machinery, the composite-key ordering, and the expected result are
/// all sound; the only difference from the finding below is whether `tag` reads
/// the source field or the like-valued sibling key output.
#[test]
fn composite_key_component_reading_source_field_groups() {
    let (scope, env, dot) = lines_scope();
    let source = r#".lines { $key: [acct, tag], acct: account, tag: account + "-x" }"#;
    let result = eval(&scope, &env, &dot, source);
    assert_eq!(rows_fields(&result), expected_groups());
}

/// FINDING — the SAME grouped view, but the second `$key` component reads the
/// first `$key` OUTPUT (`tag: acct + "-x"`) instead of the source field. §7.1
/// permits this output-to-output reference and §7.2 admits it (both are key
/// values). It MUST group identically to the control: accounts `a` and `b` form
/// two groups keyed `(a, a-x)` and `(b, b-x)`.
///
/// This test FAILS against the current implementation: `group_key` never binds
/// the `acct` output it just computed, so evaluating `tag`'s `acct + "-x"`
/// raises an unbound-`LocalBinding` error and the whole read faults.
#[test]
fn composite_key_component_reading_sibling_key_output_groups() {
    let (scope, env, dot) = lines_scope();
    let source = r#".lines { $key: [acct, tag], acct: account, tag: acct + "-x" }"#;
    let result = match try_eval(&scope, &env, &dot, source) {
        Ok(cell) => cell,
        Err(err) => panic!(
            "§7.1/§7.2: a grouped view whose `$key` component reads a sibling key output \
             (`tag: acct + \"-x\"`) must group, not fault — `group_key` failed to bind the \
             computed `acct` output: {}",
            err.message(),
        ),
    };
    assert_eq!(
        rows_fields(&result),
        expected_groups(),
        "the output-to-output key reference must yield the same groups as the control",
    );
}
