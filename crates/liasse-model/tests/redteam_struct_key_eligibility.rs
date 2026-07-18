#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team: Annex A.8 key-eligibility of a *struct* key field.
//!
//! A.8 (SPEC.md line 4472) enumerates the types a collection key field MAY use
//! and lists, next to the scalars, "structs composed solely of key-eligible
//! required fields". A struct whose members are all key-eligible and required is
//! therefore a valid `$key`, and `Type::is_key_eligible` in `liasse-value`
//! already agrees (`Self::Struct(fields) => fields.all(is_key_eligible)`).
//!
//! But the model's `$key` acceptance (`liasse-model/src/build/keys.rs`,
//! `key_field_type`) admits only a `Node::Scalar` (writable) or a `Node::Reference`
//! field and routes every other node — including the `Node::Struct` an inline
//! struct field builds to (`build/shapes.rs`) — to a catch-all rejection ("must be
//! a writable scalar or a required ref field"). So the model rejects EVERY struct
//! key, valid or not, contradicting A.8's explicit allowance. This test pins the
//! accept case A.8 grants; it fails on the buggy build and passes once
//! `key_field_type` consults `is_key_eligible` for a `Node::Struct`.
//!
//! The corpus sibling `annex-a-types-wire/red/json-type-in-struct-key-excluded`
//! (a struct key with a `json` member, expected `invalid`) currently passes only
//! *by accident* of this same defect — the model rejects it for "not a scalar",
//! not for the json member — so the exclusion is unverified until the accept side
//! works.

mod common;
use common::build;

/// A.8 accept: a `$key` naming a struct of two required key-eligible fields
/// (`int` + `text`) MUST load — the struct is "composed solely of key-eligible
/// required fields".
#[test]
fn struct_key_of_eligible_required_fields_loads() {
    let package = r#"{
      "$liasse": 1,
      "$app": "t.structkey.ok@1.0.0",
      "$model": {
        "cells": {
          "$key": "loc",
          "loc": { "x": "int", "y": "text" },
          "value": "text"
        }
      }
    }"#;
    let built = build(package);
    if let Err(diags) = &built.result {
        panic!(
            "A.8 allows a struct of key-eligible required fields as a `$key`, but the \
             model rejected it:\n{}",
            diags.render(&built.sources)
        );
    }
}

/// A.8 accept, composite form: a struct component sitting *inside* a composite
/// `$key` (`["region", "loc"]`) is equally admissible — a composite key "combines
/// several eligible fields" and a struct of eligible required fields is one such
/// eligible field. This isolates the same defect in the composite-key path.
#[test]
fn composite_key_with_struct_component_loads() {
    let package = r#"{
      "$liasse": 1,
      "$app": "t.structkey.composite@1.0.0",
      "$model": {
        "cells": {
          "$key": ["region", "loc"],
          "region": "text",
          "loc": { "x": "int", "y": "int" },
          "value": "text"
        }
      }
    }"#;
    let built = build(package);
    if let Err(diags) = &built.result {
        panic!(
            "A.8 allows a struct of key-eligible required fields as a composite-key \
             component, but the model rejected it:\n{}",
            diags.render(&built.sources)
        );
    }
}

/// A.8 reject (masked sibling, now unmasked): a struct `$key` with a `json`
/// member is NOT "composed solely of key-eligible required fields" — nesting does
/// not launder an ineligible type into a key. Before the fix this package was
/// rejected by accident (every struct key was refused as "not a scalar/ref"), so
/// the exclusion was unverified. Now that a valid struct key loads, the exclusion
/// must reject for the *right* reason: it must name the ineligible `meta`/`json`
/// member, not the blanket "must be a writable scalar" message that would fire
/// for any struct.
#[test]
fn struct_key_with_json_member_rejected_for_the_json_member() {
    let package = r#"{
      "$liasse": 1,
      "$app": "t.structjsonkey@1.0.0",
      "$model": {
        "docs": {
          "$key": "loc",
          "loc": { "area": "text", "meta": "json" },
          "body": "text"
        }
      }
    }"#;
    let built = build(package);
    let rendered = built.rendered();
    assert!(
        built.has_code("M-KEY"),
        "a struct key with a json member must be rejected with the key code (A.8):\n{rendered}"
    );
    // The diagnostic must pin the offending member, not fire the generic
    // "not a scalar/ref" struct-blanket refusal that masked this case before.
    assert!(
        rendered.contains("meta") && rendered.contains("json") && rendered.contains("not key-eligible"),
        "the rejection must name the ineligible `meta`/`json` struct member (A.8):\n{rendered}"
    );
    assert!(
        !rendered.contains("must be a writable scalar"),
        "the struct key must be rejected for its json member, not the blanket \
         'must be a writable scalar' struct refusal that masked this before:\n{rendered}"
    );
}
