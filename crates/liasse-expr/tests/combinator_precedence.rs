#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! View-combinator precedence and grouping (SPEC-ISSUES #25, §7.4).
//!
//! The pinned rule: `|` (union) and `&` (intersection) share one precedence
//! level. A chain repeating a single combinator is left-associative and
//! well-formed; a chain MIXING `|` and `&` without `( )` grouping is ambiguous —
//! the groupings differ observably in order, projection, and identity (§7.4) —
//! and is a static error. Grouping uses `( )` (CEL syntax, §6.1). These
//! expectations follow from the resolution, not from the implementation.

mod common;

use common::{check, check_rejects, row_type, scalar, view, FixedScope};
use liasse_expr::ExprType;
use liasse_value::Type;

/// A root exposing three keyed views `a`/`b`/`c` sharing one identity domain, so
/// any combination of them type-checks on identity grounds and only precedence
/// is under test.
fn scope() -> FixedScope {
    let item = row_type(vec![("id", scalar(Type::Text))], Some(scalar(Type::Text)));
    let root = row_type(
        vec![("a", view(item.clone())), ("b", view(item.clone())), ("c", view(item))],
        None,
    );
    FixedScope::new(ExprType::Row(root))
}

#[test]
fn mixed_union_and_intersection_without_grouping_is_rejected() {
    let diags = check_rejects(&scope(), ".a | .b & .c");
    let text = format!("{diags:?}");
    assert!(
        text.contains("ambiguous") || text.contains('('),
        "the diagnostic must explain the ambiguity and point at `( )` grouping, got: {text}"
    );
}

#[test]
fn mixed_chain_the_other_way_is_also_rejected() {
    // Order of the two kinds does not matter: any mix is ambiguous.
    check_rejects(&scope(), ".a & .b | .c");
}

#[test]
fn repeated_single_combinator_is_left_associative_and_valid() {
    // A homogeneous chain stays well-formed (left-associative), so neither of
    // these repeated-combinator chains is rejected.
    check(&scope(), ".a | .b | .c");
    check(&scope(), ".a & .b & .c");
}

#[test]
fn parentheses_disambiguate_a_mixed_chain() {
    // Explicit `( )` grouping (§6.1) makes each combination node homogeneous, so
    // both groupings of the once-ambiguous chain now type-check.
    check(&scope(), "(.a | .b) & .c");
    check(&scope(), ".a | (.b & .c)");
}
