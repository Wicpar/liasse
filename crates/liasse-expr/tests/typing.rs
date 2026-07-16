#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Static typing rejections (§6, §7). Each asserts the package-load-time
//! diagnostic the spec mandates, deduced from the rule cited in the test name.

mod common;

use common::{check_rejects, row_type, scalar, view, FixedScope};
use liasse_diag::Diagnostics;
use liasse_expr::ExprType;
use liasse_value::Type;

/// A root row exposing a keyed `lines`/`items` collection plus a couple of
/// scalar fields, mirroring the corpus models.
fn model_scope() -> FixedScope {
    let line = row_type(
        vec![
            ("id", scalar(Type::Text)),
            ("account", scalar(Type::Text)),
            ("debit", scalar(Type::Int)),
        ],
        Some(scalar(Type::Text)),
    );
    let root = row_type(
        vec![
            ("qty", scalar(Type::Int)),
            ("label", scalar(Type::Text)),
            ("price", scalar(Type::Int)),
            ("discount", scalar(Type::Optional(Box::new(Type::Int)))),
            ("lines", view(line)),
        ],
        None,
    );
    FixedScope::new(ExprType::Row(root))
}

fn mentions(diags: &Diagnostics, needle: &str) -> bool {
    diags.iter().any(|d| d.message().contains(needle))
}

#[test]
fn adding_int_and_text_has_no_type() {
    // §6.1 static typing: `int + text` is rejected at load.
    let diags = check_rejects(&model_scope(), ".qty + .label");
    assert!(diags.has_errors());
    assert!(mentions(&diags, "no type for operands"));
}

#[test]
fn unknown_name_is_rejected() {
    let diags = check_rejects(&model_scope(), "nonesuch");
    assert!(mentions(&diags, "unknown name"));
}

#[test]
fn unknown_function_is_rejected() {
    // §6.5: package loading validates every function name.
    let diags = check_rejects(&model_scope(), "bogus(.qty)");
    assert!(mentions(&diags, "unknown function"));
}

#[test]
fn arithmetic_over_optional_operand_is_rejected() {
    // SPEC-ISSUES item 3 documented choice: an optional operand is a static
    // type error (coalesce first).
    let diags = check_rejects(&model_scope(), ".price - .discount");
    assert!(mentions(&diags, "optional"));
}

#[test]
fn structural_binding_absent_from_context_is_rejected() {
    // §6.2: a structural binding exists only in its feature context; `$config`
    // is absent here, so the scope resolves it to nothing.
    let diags = check_rejects(&model_scope(), "$config");
    assert!(mentions(&diags, "not available"));
}

#[test]
fn grouped_nonaggregated_source_value_is_rejected() {
    // §7.2: every non-key source value must be aggregated or key-derived.
    let diags = check_rejects(&model_scope(), ".lines { $key: account, account, debit }");
    assert!(mentions(&diags, "neither"));
}

#[test]
fn grouped_aggregate_output_is_accepted() {
    // The valid counterpart: `debit` becomes an aggregate over `group`.
    let scope = model_scope();
    let typed = common::check(&scope, ".lines { $key: account, account, total: sum(group.debit) }");
    assert!(matches!(typed.ty(), ExprType::View(_)));
}

#[test]
fn projection_output_cycle_is_rejected() {
    // §7.1: cross-references must be acyclic.
    let diags = check_rejects(&model_scope(), ".lines { a: b, b: a }");
    assert!(mentions(&diags, "cycle"));
}

#[test]
fn negative_skip_bound_is_rejected() {
    // §7.3: `$skip`/`$limit` are non-negative.
    let diags = check_rejects(&model_scope(), ".lines { id, $skip: -1 }");
    assert!(mentions(&diags, "non-negative"));
}

#[test]
fn comparing_a_scalar_with_a_view_is_rejected() {
    let diags = check_rejects(&model_scope(), ".qty == .lines");
    assert!(diags.has_errors());
}

#[test]
fn summing_a_non_numeric_field_is_rejected() {
    // §7.5: `sum` returns the field's numeric type, so a `text` field has no
    // sum. Without this the checker would type `sum(.lines.account)` as `text`
    // while evaluation returns an integer zero — an unsound result.
    let diags = check_rejects(&model_scope(), "sum(.lines.account)");
    assert!(mentions(&diags, "numeric"));
    let diags = check_rejects(&model_scope(), "avg(.lines.id)");
    assert!(mentions(&diags, "numeric"));
}
