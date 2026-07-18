//! Nested-collection ref key type (SPEC-ISSUES #26, §5.4/§D.1/§A.9).
//!
//! A `$ref` to a nested collection carries the target row's *full identity*: the
//! ordered composite of every ancestor collection `$key` followed by the target's
//! local `$key`, in ancestor-then-local order (§5.4, §D.1), wire-encoded as the
//! §A.9 composite array. A ref to a *root* collection is unchanged — its local
//! `$key` only — and the dot-separated target spelling (`/companies.offices`) is
//! not a valid ref path, so it resolves to no collection and is rejected.
//!
//! Every expected type is read off the model's own `$key` declarations
//! (`companies.$key = id`, `offices.$key = id`), not the resolver's own answer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use common::build;
use liasse_model::{Model, Node};
use liasse_value::Type;

/// The resolved key type of the `$ref` field `field` on collection `collection`.
fn ref_key_type<'a>(model: &'a Model, collection: &str, field: &str) -> &'a Type {
    let coll = model
        .root()
        .member(collection)
        .unwrap_or_else(|| panic!("collection `{collection}` is present"));
    let Node::Collection(collection_body) = &coll.node else {
        panic!("`{collection}` is a keyed collection");
    };
    let member = collection_body
        .shape
        .member(field)
        .unwrap_or_else(|| panic!("field `{field}` is present on `{collection}`"));
    let Node::Reference(reference) = &member.node else {
        panic!("`{field}` is a `$ref`");
    };
    &reference.key_type
}

const NESTED: &str = r#"{
  "$liasse": 1,
  "$app": "t.nestedref@1.0.0",
  "$model": {
    "companies": {
      "$key": "id",
      "id": "text",
      "offices": { "$key": "id", "id": "text" }
    },
    "links": {
      "$key": "id",
      "id": "text",
      "office": { "$ref": "/companies/offices" },
      "org": { "$ref": "/companies" }
    }
  }
}"#;

#[test]
fn nested_collection_ref_carries_ancestor_then_local_composite_key() {
    // /companies/offices: full identity is companies.$key (`id: text`) THEN the
    // office's local $key (`id: text`), in ancestor-then-local order.
    let built = build(NESTED);
    let key = ref_key_type(built.expect_ok(), "links", "office");
    assert_eq!(
        *key,
        Type::Composite(vec![
            ("companies.id".to_owned(), Type::Text),
            ("id".to_owned(), Type::Text),
        ]),
        "a ref to a nested collection carries the ancestor-then-local composite key"
    );
}

#[test]
fn root_collection_ref_key_is_unchanged_local_key_only() {
    // A root collection has no ancestors, so its ref key stays its local scalar
    // $key — the composite change is scoped to nested targets.
    let built = build(NESTED);
    let key = ref_key_type(built.expect_ok(), "links", "org");
    assert_eq!(*key, Type::Text, "a root-collection ref carries its local $key only");
}

#[test]
fn dot_separated_nested_target_is_not_a_valid_ref_path() {
    // `/companies.offices` names no collection under the /-separated index, so it
    // is rejected as an unresolvable target (§5.6/§C.5); the valid spelling is the
    // /-separated `/companies/offices`.
    let built = build(
        r#"{
          "$liasse": 1,
          "$app": "t.nestedrefdot@1.0.0",
          "$model": {
            "companies": {
              "$key": "id",
              "id": "text",
              "offices": { "$key": "id", "id": "text" }
            },
            "links": {
              "$key": "id",
              "id": "text",
              "office": { "$ref": "/companies.offices" }
            }
          }
        }"#,
    );
    assert!(built.has_code(liasse_model::code::REF));
    assert!(built.points_at("/companies.offices"));
}
