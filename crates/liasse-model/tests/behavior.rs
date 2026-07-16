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

#[test]
fn param_inferred_from_assignment_target_with_optionality() {
    // §8.3: "CEL typing infers a parameter from every use of `@name`" and
    // "`@name` inherits `.name`'s type and optionality" — the spec's own
    // `"rename": ".name = @name"` example against an optional text field.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.inferassign@1.0.0"
          "$model": {
            "people": {
              "$key": "id"
              "id": "text"
              "name": "text?"
              "$mut": { "rename": ".name = @name" }
            }
          }
        }"#,
    );
    let model = built.expect_ok();
    let rename = model
        .mutations()
        .iter()
        .find(|m| m.name.as_str() == "rename")
        .expect("rename present");
    let name = rename
        .params
        .iter()
        .find(|(name, _)| name == "name")
        .expect("@name inferred");
    // Optionality is inherited, not stripped: the contract type is `text?`.
    assert_eq!(
        name.1.as_scalar(),
        Some(&liasse_value::Type::Optional(Box::new(liasse_value::Type::Text)))
    );
}

#[test]
fn param_inferred_from_collection_key_selector() {
    // §8.3: "`@id` inherits `.tasks.$key`" — the spec's own
    // `"complete": ".tasks[@id].done = true"` example.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.inferkey@1.0.0"
          "$model": {
            "tasks": {
              "$key": "id"
              "id": "text"
              "done": "bool = false"
            }
            "$mut": { "complete": ".tasks[@id].done = true" }
          }
        }"#,
    );
    let model = built.expect_ok();
    let complete = model
        .mutations()
        .iter()
        .find(|m| m.name.as_str() == "complete")
        .expect("complete present");
    let id = complete
        .params
        .iter()
        .find(|(name, _)| name == "id")
        .expect("@id inferred");
    assert_eq!(id.1.as_scalar(), Some(&liasse_value::Type::Text));
}

#[test]
fn uninferable_unprototyped_param_rejected() {
    // §8.3: "All uses of the same parameter MUST infer one compatible type",
    // and "An explicit prototype resolves ambiguity or declares a structure
    // that the body cannot uniquely infer." `return @value` constrains @value
    // to no type, no prototype is declared, so no single contract type exists
    // (the parameter shape "is part of the external surface contract") and the
    // package must not load.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.noinfer@1.0.0"
          "$model": {
            "$mut": { "echo": "return @value" }
          }
        }"#,
    );
    assert!(built.has_code(code::MUTATION));
    // The diagnostic names the parameter at its use...
    assert!(built.points_at("@value"));
    assert!(built
        .expect_err()
        .iter()
        .any(|d| d.message().contains("@value") && d.message().contains("cannot be inferred")));
    // ...and hints at the prototype form that §8.3 provides for this case.
    assert!(built
        .expect_err()
        .iter()
        .any(|d| d.helps().iter().any(|h| h.contains("prototype"))));
}
