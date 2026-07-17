#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Red-team probe: view-combinator identity domain over `::` traversals
//! (§7.2 composed identity, §7.4 union/intersection).

mod common;

use common::{collection, eval, keyed_row, keyless_row, row_type, scalar, scell, vtext, view, FixedEnv, FixedScope};
use liasse_expr::{Cell, ExprType};
use liasse_value::Type;

/// Root with two top-level collections `red` and `blue`, each carrying a nested
/// `items` collection.  Both nested collections contain an item keyed `"1"`.
fn two_parents() -> (FixedScope, FixedEnv, Cell) {
    let item_ty = row_type(
        vec![("id", scalar(Type::Text)), ("label", scalar(Type::Text))],
        Some(scalar(Type::Text)),
    );
    let parent_ty = row_type(
        vec![("id", scalar(Type::Text)), ("items", view(item_ty))],
        Some(scalar(Type::Text)),
    );
    let root_ty = row_type(
        vec![("red", view(parent_ty.clone())), ("blue", view(parent_ty))],
        None,
    );
    let scope = FixedScope::new(ExprType::Row(root_ty));

    let item = |k: &str, label: &str| {
        keyed_row(k, vtext(k), vec![("id", scell(vtext(k))), ("label", scell(vtext(label)))])
    };
    let parent = |k: &str, item_row: liasse_expr::Row| {
        keyed_row(k, vtext(k), vec![("id", scell(vtext(k))), ("items", collection(vec![item_row]))])
    };
    let root = keyless_row(
        0,
        vec![
            ("red", collection(vec![parent("r", item("1", "Red1"))])),
            ("blue", collection(vec![parent("b", item("1", "Blue1"))])),
        ],
    );
    let dot = Cell::Row(Box::new(root.clone()));
    (scope, FixedEnv::new(root), dot)
}

#[test]
fn union_over_traversals_keeps_distinct_composed_identities() {
    // §7.2: a `::` traversal inherits `outer.$key + inner.$key`, so the item under
    // `red` (identity `r + 1`) and the item under `blue` (identity `b + 1`) are two
    // DISTINCT view identities even though both inner keys are `"1"`. §7.4: union is
    // "left order then new right identities" — the blue item's identity is new, so
    // the union MUST contain both rows. Externally derived from the identity rule.
    let (scope, env, dot) = two_parents();
    let result = eval(&scope, &env, &dot, ".red::items { label } | .blue::items { label }");
    let labels: Vec<_> = common::rows_fields(&result)
        .into_iter()
        .flat_map(|r| r.into_iter().map(|(_, v)| v))
        .collect();
    assert_eq!(
        labels,
        vec![vtext("Red1"), vtext("Blue1")],
        "union must keep both composed identities (r+1 and b+1), not dedup by inner key"
    );
}
