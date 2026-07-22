#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team W4-F5 — §6.1 expression nesting depth counts EVERY recursion form,
//! not only bracketed groups.
//!
//! The load pipeline caps nesting to protect its recursive passes (`pest`, the
//! `liasse-expr` checker `check`/`check_unary`/`check_binary`, and the evaluator)
//! from a stack overflow. The original cap counted only bracket characters
//! (`([{`), so a *bracket-free* chain slipped past it however deep:
//!
//! - a unary run `!!!!…x` (or `----…x`),
//! - a field-access chain `.a.a.a…`,
//! - a same-name traversal `.a::a::a…`,
//! - a left-associative binary chain `x+x+x…`,
//! - a generic type tower `optional<optional<…>>` (which nests through `<`/`>`).
//!
//! Each admitted an arbitrarily deep AST that the recursive checker/evaluator then
//! walked one stack frame per node — a SIGABRT at load (the same crash class as
//! the bracketed cap in commit fcaa68a, and forbidden by AGENTS.md "code must
//! never panic"). Every case below MUST now be a clean, bounded `invalid` load
//! rejection. Reaching each assertion at all proves no overflow occurred.

use liasse_runtime::{Engine, EngineError, FixedGenerators, Precision};
use liasse_store::MemoryStore;

/// Load a package definition, returning the invalid-load diagnostic messages
/// (`Err`) or `Ok(())` on success. Used to assert the *cause* of a rejection.
fn load_diagnostics(name: &str, package: &serde_json::Value) -> Result<(), Vec<String>> {
    let definition = serde_json::to_string(package).expect("serialize");
    let store = MemoryStore::new(liasse_ident::InstanceId::new(name.to_owned()));
    let mut generator = FixedGenerators::new(0, Precision::Micros);
    match Engine::load(store, &definition, &mut generator) {
        Ok(_) => Ok(()),
        Err(EngineError::Invalid(diags)) => {
            Err(diags.iter().map(|d| d.message().to_owned()).collect())
        }
        Err(other) => Err(vec![format!("non-invalid engine error: {other}")]),
    }
}

/// A package whose sole `$public` view projects `d: {body}` over the row.
fn view_package(app: &str, body: &str) -> serde_json::Value {
    serde_json::json!({
        "$liasse": 1, "$app": app,
        "$model": {
            "items": { "$key": "id", "id": "text" },
            "$public": { "deep": { "$view": format!(". {{ d: {body} }}") } }
        }
    })
}

/// Assert a view whose projected expression is `body` is rejected at load for a
/// nesting-depth reason, never accepted and never a crash.
fn assert_view_depth_rejected(name: &str, app: &str, body: &str) {
    match load_diagnostics(name, &view_package(app, body)) {
        Ok(()) => panic!("{name}: a bracket-free depth-{{cap+}} expression must be rejected"),
        Err(msgs) => assert!(
            msgs.iter().any(|m| m.contains("nests") && m.contains("32")),
            "{name}: expected a nesting-depth rejection (no crash), got {msgs:?}"
        ),
    }
}

#[test]
fn unary_chain_view_rejected_at_load() {
    // `!!!!…false` — no bracket, 40 deep.
    assert_view_depth_rejected("w4-unary", "t.w4unary@1.0.0", &format!("{}false", "!".repeat(40)));
}

#[test]
fn field_chain_view_rejected_at_load() {
    // `.a.a.a…` — a field tower over the row.
    let body = format!(".{}", "a.".repeat(40).trim_end_matches('.'));
    assert_view_depth_rejected("w4-field", "t.w4field@1.0.0", &body);
}

#[test]
fn binary_chain_view_rejected_at_load() {
    // `false||false||…` — a left-associative logical chain.
    let body = "false".to_string() + &" || false".repeat(40);
    assert_view_depth_rejected("w4-binary", "t.w4binary@1.0.0", &body);
}

#[test]
fn unary_chain_view_at_pathological_depth_rejects_without_crashing() {
    // 50 000 deep: pre-fix this SIGABRTed during load (checker overflow, then the
    // recursive `Drop` freeing the rejected tree). It must reject cleanly.
    assert_view_depth_rejected(
        "w4-unary-huge",
        "t.w4unaryhuge@1.0.0",
        &format!("{}false", "!".repeat(50_000)),
    );
}

#[test]
fn generic_type_tower_rejected_at_load() {
    // A deep `optional<…>` field type nests through `<`/`>`, driving `pest` and
    // the model's recursive type lowering; it must be rejected before either runs.
    let ty = format!("{}text{}", "optional<".repeat(40), ">".repeat(40));
    let package = serde_json::json!({
        "$liasse": 1, "$app": "t.w4type@1.0.0",
        "$model": { "items": { "$key": "id", "id": "text", "f": ty } }
    });
    match load_diagnostics("w4-type", &package) {
        Ok(()) => panic!("w4-type: a depth-{{cap+}} generic type must be rejected"),
        Err(msgs) => assert!(
            msgs.iter().any(|m| m.contains("nests") && m.contains("32")),
            "w4-type: expected a nesting-depth rejection (no crash), got {msgs:?}"
        ),
    }
}

#[test]
fn shallow_bracket_free_chain_loads() {
    // CONTROL: the same construct at a shallow depth is a valid view and loads
    // cleanly, isolating the defect to DEPTH rather than to the chain itself.
    match load_diagnostics("w4-shallow", &view_package("t.w4shallow@1.0.0", "!!false")) {
        Ok(()) => {}
        Err(msgs) => panic!("w4-shallow: a shallow `!!false` view must load, got {msgs:?}"),
    }
}
