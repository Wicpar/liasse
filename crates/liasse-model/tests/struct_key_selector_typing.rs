#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! A struct-typed `$key` (Annex A.8) is a *typed value*, so a selector that
//! addresses a collection by that key MUST type its operand against the struct
//! type (§5.4, §6.3). Accepting the struct key (commit a4495e9) fixed the data
//! flow — insert/scan/rekey carry a `Value::Struct` key — but left the *declared*
//! key type falling back to `json` in the resolver/runtime-schema key-type
//! computation, so a struct-key selector operand could not be typed: a parameter
//! declared with the struct type but used as a `.cells[@k]` selector inferred two
//! incompatible types (declared `struct` vs the `json` key type) and the package
//! failed to load.
//!
//! These tests pin the completed behavior: a struct `$key`'s declared key type is
//! the struct itself (field-name ordered), matching the value the store carries,
//! so a selector operand types against it — as a bare parameter and as an object
//! literal naming each member. Every expected type is derived from the model's
//! own `$key` declaration (`loc = { x: int, y: int }`), not from the engine's
//! answer.

mod common;
use common::build;

use liasse_model::Model;
use liasse_value::{StructType, Type};

/// The scalar type inferred for `@param` of mutation `mutation`, panicking (test
/// failure) if the mutation or parameter is absent or the type is non-scalar.
fn param_type<'a>(model: &'a Model, mutation: &str, param: &str) -> &'a Type {
    let mutation = model
        .mutations()
        .iter()
        .find(|m| m.name.as_str() == mutation)
        .unwrap_or_else(|| panic!("mutation `{mutation}` is present in the loaded model"));
    let (_, ty) = mutation
        .params
        .iter()
        .find(|(name, _)| name == param)
        .unwrap_or_else(|| panic!("parameter `@{param}` is in the contract: {:?}", mutation.params));
    ty.as_scalar()
        .unwrap_or_else(|| panic!("parameter `@{param}` is a scalar-valued key, got {ty:?}"))
}

/// The struct key type `{ x: int, y: int }` as A.8 declares it — field-name
/// (text) ordered, the exact `Type::Struct` the store carries for the key.
fn loc_struct_type() -> Type {
    Type::Struct(StructType::new([
        ("x".to_owned(), Type::Int),
        ("y".to_owned(), Type::Int),
    ]))
}

/// A collection keyed by a struct `loc = { x: int, y: int }`, whose `$mut`
/// program `body` is spliced in. Every test drives the same collection so the
/// only variable is how the mutation addresses the struct key.
fn package(mutations: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "t.structkey.selector@1.0.0",
          "$model": {{
            "cells": {{
              "$key": "loc",
              "loc": {{ "x": "int", "y": "int" }},
              "value": "text"
            }},
            "$mut": {mutations}
          }}
        }}"#
    )
}

/// A.8/§6.3: a bare `@k` selecting `.cells[@k]` inherits the collection's key
/// type. That key type is the struct `{ x: int, y: int }`, not `json` — so the
/// parameter types as the struct. Before the key-type fix `@k` inferred `json`
/// (the resolver's non-scalar-key fallback), so this asserts the struct type the
/// old fallback could not produce.
#[test]
fn bare_struct_key_selector_types_the_param_as_the_struct() {
    let built = build(&package(
        r#"{ "pick": ["removed = .cells[@k]", ".cells - @k", "return removed"] }"#,
    ));
    let model = built.expect_ok();
    assert_eq!(
        param_type(model, "pick", "k"),
        &loc_struct_type(),
        "a `.cells[@k]` selector over a struct-keyed collection types `@k` as the struct key \
         `{{ x: int, y: int }}`, not the `json` fallback"
    );
}

/// §8.3 regression the fix closes: a parameter *declared* with the struct key
/// type and *used* as a struct-key selector must load — pre-fix the selector
/// inferred `json` while the prototype declared `struct`, so `@k` was "used with
/// two incompatible types" and the package was rejected. Post-fix both agree on
/// the struct, so it loads. (This is the exact failure the completion targets.)
#[test]
fn declared_struct_key_used_as_selector_loads() {
    let built = build(&package(
        r#"{ "grab({ k: { x: int, y: int } })": ".cells - @k" }"#,
    ));
    if let Err(diags) = &built.result {
        panic!(
            "a parameter declared with the struct key type and used as a struct-key selector \
             must load (§8.3, A.8), but it was rejected:\n{}",
            diags.render(&built.sources)
        );
    }
    // The declared struct is the same struct the selector infers, so it settles
    // to exactly that type.
    assert_eq!(param_type(built.expect_ok(), "grab", "k"), &loc_struct_type());
}

/// §6.3/A.9: an object key selector `[{ x: @x, y: @y }]` names each key member,
/// so each parameter inherits that member's type from the struct key — `@x` and
/// `@y` are both `int`. Pre-fix the key type was `json`, which has no addressable
/// members, so `@x`/`@y` could not be inferred and the mutation was rejected
/// ("cannot be inferred to a single type"); post-fix each member types.
#[test]
fn object_struct_key_selector_infers_each_member() {
    let built = build(&package(r#"{ "at": ".cells - { x: @x, y: @y }" }"#));
    let model = built.expect_ok();
    assert_eq!(
        param_type(model, "at", "x"),
        &Type::Int,
        "the object selector `{{ x: @x, y: @y }}` types `@x` as the struct member `x` (int)"
    );
    assert_eq!(
        param_type(model, "at", "y"),
        &Type::Int,
        "the object selector `{{ x: @x, y: @y }}` types `@y` as the struct member `y` (int)"
    );
}
