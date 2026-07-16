//! Mutation, surface, and seed rejections/acceptance (§8, §10, §5/§9).

// Tests are expected to panic on failure (AGENTS.md).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;
use liasse_model::code;

#[test]
fn write_to_computed_value_rejected() {
    // §5.2/§8.5: a mutation may not assign to a read-only computed value.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.forge@1.0.0"
          "$model": {
            "invoices": {
              "$key": "id"
              "id": "text"
              "subtotal": "int"
              "tax": "int"
              "total": "= .subtotal + .tax"
              "$mut": { "forge": ".total = @total" }
            }
          }
        }"#,
    );
    assert!(built.has_code(code::MUTATION));
    assert!(built.points_at(".total"));
    assert!(built.has_hint());
}

#[test]
fn return_not_final_statement_rejected() {
    // §8.10: `return` may appear only as the final statement.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.ret@1.0.0"
          "$model": {
            "tasks": {
              "$key": "id"
              "id": "text"
              "done": "bool = false"
              "$mut": { "bad": ["return .done", ".done = true"] }
            }
          }
        }"#,
    );
    assert!(built.has_code(code::MUTATION));
    assert!(built.has_hint());
}

#[test]
fn assert_condition_must_be_bool() {
    // §8.8: an assert condition is a bool.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.assert@1.0.0"
          "$model": {
            "count": "int"
            "$mut": { "check_it": "assert(.count, 'nope')" }
          }
        }"#,
    );
    assert!(built.has_code(code::MUTATION));
}

#[test]
fn surface_exposes_undeclared_mutation_rejected() {
    // §10.1: a surface mutation reference must name a declared mutation.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.surf@1.0.0"
          "$model": {
            "tasks": { "$key": "id", "id": "text" }
            "$public": {
              "s": { "$mut": { "go": ".does_not_exist" } }
            }
          }
        }"#,
    );
    assert!(built.has_code(code::SURFACE));
    assert!(built.has_hint());
}

#[test]
fn explicit_prototype_declares_parameter() {
    // §8.3: an explicit prototype declares a parameter the body cannot infer.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.proto@1.0.0"
          "$model": {
            "settings": {
              "$key": "id"
              "id": "text"
              "note": "text"
              "$mut": { "set_note({ note: text })": ".note = @note" }
            }
          }
        }"#,
    );
    let model = built.expect_ok();
    let set_note = model
        .mutations()
        .iter()
        .find(|m| m.name.as_str() == "set_note")
        .expect("set_note present");
    let note = set_note
        .params
        .iter()
        .find(|(name, _)| name == "note")
        .expect("prototype parameter present");
    assert_eq!(note.1.describe(), "text");
}

#[test]
fn seed_value_type_mismatch_rejected() {
    // §5/§9: a seed value must conform to the declared field type.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.seed@1.0.0"
          "$model": { "count": "int" }
          "$data": { "count": "not-a-number" }
        }"#,
    );
    assert!(built.has_code(code::SEED));
    assert!(built.has_hint());
}

#[test]
fn seed_value_conforms_accepted() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.seedok@1.0.0"
          "$model": { "count": "int" }
          "$data": { "count": "42" }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn multiple_errors_accumulated() {
    // The builder reports every problem, not just the first.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.multi@1.0.0"
          "$model": {
            "_bad": "text"
            "things": { "$key": "missing", "status": { "$enum": ["a", "a"] } }
          }
        }"#,
    );
    assert!(built.has_code(code::NAME_GRAMMAR));
    assert!(built.has_code(code::KEY));
    assert!(built.has_code(code::ENUM));
}
