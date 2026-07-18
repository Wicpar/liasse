#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team: Annex A.8 key-eligibility of a *struct* `$unique` candidate-key
//! component.
//!
//! A.8 (SPEC.md line 4475) states: "Candidate-key components use the same
//! eligible base types, although the candidate-key fields themselves MAY be
//! optional". The "same eligible base types" are the types A.8 lists two lines
//! above for a `$key` field — the scalars *and* "structs composed solely of
//! key-eligible required fields". So a struct of all-eligible required fields is
//! an eligible `$unique` candidate-key component exactly as it is an eligible
//! `$key` component, and `Type::is_key_eligible` in `liasse-value` already agrees
//! (`Self::Struct(fields) => fields.all(is_key_eligible)`).
//!
//! But the model's candidate-key acceptance
//! (`liasse-model/src/build/keys.rs`, `unique_field`) matches only a
//! `Node::Scalar` field or a `Node::Reference` field and routes every other node
//! — including the `Node::Struct` an inline struct field builds to
//! (`build/shapes.rs`) — to a catch-all rejection ("candidate-key field `{name}`
//! must be a scalar or ref field"). The primary-`$key` path (`key_field_type`,
//! same file) was updated for struct keys and DOES accept a `Node::Struct`; the
//! `$unique` path was not. So the model rejects EVERY struct candidate key, valid
//! or not, contradicting A.8 — a struct is a valid `$key` but not a valid
//! `$unique` on the very same shape.
//!
//! These tests pin the accept side A.8 grants for candidate keys. They fail on
//! the current build (which rejects a struct candidate key at `unique_field`'s
//! `_ =>` arm) and pass once `unique_field` judges a `Node::Struct` component by
//! `is_key_eligible` the way `key_field_type` already does.

mod common;
use common::build;

/// A.8 accept, single-field candidate: a `$unique` naming a struct of two
/// required key-eligible fields (`int` + `int`) MUST load — `coord` is "composed
/// solely of key-eligible required fields", and A.8 grants candidate-key
/// components "the same eligible base types" as `$key` fields. The primary `$key`
/// here is an ordinary scalar, so the ONLY thing under test is the struct
/// `$unique` component.
#[test]
fn struct_unique_candidate_of_eligible_required_fields_loads() {
    let package = r#"{
      "$liasse": 1,
      "$app": "t.structunique.ok@1.0.0",
      "$model": {
        "places": {
          "$key": "id",
          "$unique": ["coord"],
          "id": "uuid = uuid()",
          "name": "text",
          "coord": { "lat": "int", "lon": "int" }
        }
      }
    }"#;
    let built = build(package);
    if let Err(diags) = &built.result {
        panic!(
            "A.8 grants candidate-key components the same eligible base types as `$key` \
             fields, which include a struct of key-eligible required fields, but the model \
             rejected a struct `$unique`:\n{}",
            diags.render(&built.sources)
        );
    }
}

/// A.8 accept, composite candidate: a struct component sitting *inside* a
/// composite candidate key (`$unique: [["region", "coord"]]`) is equally
/// admissible — a composite candidate "combines several eligible fields" and a
/// struct of eligible required fields is one such eligible field, mirroring the
/// composite `$key` path already accepted by `composite_key_with_struct_component_loads`
/// (see `redteam_struct_key_eligibility`). This isolates the same defect in the
/// composite candidate-key path.
#[test]
fn composite_unique_candidate_with_struct_component_loads() {
    let package = r#"{
      "$liasse": 1,
      "$app": "t.structunique.composite@1.0.0",
      "$model": {
        "places": {
          "$key": "id",
          "$unique": [["region", "coord"]],
          "id": "uuid = uuid()",
          "region": "text",
          "coord": { "lat": "int", "lon": "int" }
        }
      }
    }"#;
    let built = build(package);
    if let Err(diags) = &built.result {
        panic!(
            "A.8 allows a struct of key-eligible required fields as a composite candidate-key \
             component, but the model rejected it:\n{}",
            diags.render(&built.sources)
        );
    }
}

/// A.8 reject (masked second-order defect): a struct `$unique` component with a
/// `json` member is NOT "composed solely of key-eligible required fields" —
/// nesting does not launder an ineligible type into a candidate key. This package
/// IS rejected today, but for the WRONG reason: `unique_field` refuses every
/// struct at its `_ =>` arm ("must be a scalar or ref field") before ever looking
/// at the member, so the json exclusion for candidate keys is entirely unverified
/// — exactly as `json-type-in-struct-key-excluded` was masked for primary keys
/// before the struct-key fix. Once `unique_field` recurses into struct
/// eligibility, the rejection must name the ineligible `meta`/`json` member.
#[test]
fn struct_unique_candidate_with_json_member_rejected_for_the_json_member() {
    let package = r#"{
      "$liasse": 1,
      "$app": "t.structunique.json@1.0.0",
      "$model": {
        "docs": {
          "$key": "id",
          "$unique": ["loc"],
          "id": "uuid = uuid()",
          "loc": { "area": "text", "meta": "json" },
          "body": "text"
        }
      }
    }"#;
    let built = build(package);
    let rendered = built.rendered();
    assert!(
        built.has_code("M-KEY"),
        "a struct candidate key with a json member must be rejected with the key code (A.8):\n{rendered}"
    );
    // The diagnostic must pin the offending member, not fire the generic
    // "must be a scalar or ref field" struct-blanket refusal that masks this case.
    assert!(
        rendered.contains("meta") && rendered.contains("json") && rendered.contains("not key-eligible"),
        "the rejection must name the ineligible `meta`/`json` struct member (A.8), not blanket-refuse \
         every struct candidate:\n{rendered}"
    );
    assert!(
        !rendered.contains("must be a scalar or ref field"),
        "the struct candidate key must be rejected for its json member, not the blanket \
         'must be a scalar or ref field' struct refusal that masks it today:\n{rendered}"
    );
}
