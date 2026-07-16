//! Red-team regressions: each test reproduces a spec divergence found by
//! attacking the model builder, and locks in the spec-derived behavior. The
//! control tests prove the divergence was order/spelling sensitivity, not a
//! mis-derived expectation.

// Tests are expected to panic on failure (AGENTS.md).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;
use liasse_model::code;

#[test]
fn expanded_optional_before_type_key_rejected() {
    // §5.1: the members of an expanded field declaration all refine one field and
    // "their source order has no semantic effect". A.8: optional types are
    // excluded from row keys. So `id` is an optional text field whether or not
    // `$optional` precedes `$type`, and using it as a `$key` MUST be rejected
    // exactly as the `$type`-first spelling (locked in by the control below) is.
    let def = r#"{ "$liasse": 1, "$app": "t.st@1.0.0", "$model":
        { "items": { "$key": "id",
                     "id": { "$optional": true, "$type": "text" },
                     "name": "text" } } }"#;
    let built = build(def);
    assert!(
        built.has_code(code::KEY),
        "optional key field accepted when `$optional` precedes `$type`"
    );
}

#[test]
fn expanded_optional_order_control_type_first_rejected() {
    // Control: the `$type`-first spelling of the same field IS rejected, proving
    // the divergence is order-dependence, not a mis-derived expectation.
    let def = r#"{ "$liasse": 1, "$app": "t.st@1.0.0", "$model":
        { "items": { "$key": "id",
                     "id": { "$type": "text", "$optional": true },
                     "name": "text" } } }"#;
    let built = build(def);
    assert!(built.has_code(code::KEY));
}

#[test]
fn self_referential_default_rejected_as_cycle() {
    // §5.1: "Defaults and computed insertion values form one dependency graph.
    // The model is valid when that graph is acyclic." A field default that reads
    // its own field (`.n` inside `n`'s default) is a self-loop -- a cycle of
    // length one -- so it has no topological evaluation order and MUST be
    // rejected, exactly as the two-field cycle is. The `int` typing keeps
    // `.n + 1` well-typed so the only rule that can fire is the acyclicity rule.
    let def = r#"{ "$liasse": 1, "$app": "t.st@1.0.0", "$model":
        { "items": { "$key": "id", "id": "text", "n": "int = .n + 1" } } }"#;
    let built = build(def);
    assert!(
        built.has_code(code::CYCLE),
        "self-referential default accepted as acyclic"
    );
}

#[test]
fn unique_shorthand_non_key_eligible_rejected() {
    // §5.7: "Field `$unique: true` adds one single-field candidate key for that
    // field" and "Every present component MUST have a key-eligible type." A.8:
    // `json` is not key-eligible. The array spelling of the same constraint
    // (control below) is rejected, so the equivalent shorthand MUST be too.
    let def = r#"{ "$liasse": 1, "$app": "t.st@1.0.0", "$model":
        { "items": { "$key": "id", "id": "text",
                     "payload": { "$type": "json", "$unique": true } } } }"#;
    let built = build(def);
    assert!(
        built.has_code(code::KEY),
        "non-key-eligible `$unique: true` shorthand accepted"
    );
}

#[test]
fn unique_array_control_non_key_eligible_rejected() {
    // Control: the array spelling of the very same candidate key is rejected for
    // its non-key-eligible `json` component (§5.7, A.8).
    let def = r#"{ "$liasse": 1, "$app": "t.st@1.0.0", "$model":
        { "items": { "$key": "id", "$unique": ["payload"],
                     "id": "text", "payload": "json" } } }"#;
    let built = build(def);
    assert!(built.has_code(code::KEY));
}

#[test]
fn unique_shorthand_key_eligible_accepted() {
    // Control: a key-eligible `$unique: true` shorthand loads and registers a
    // candidate key, so the rejection above is about key-eligibility, not the
    // shorthand itself.
    let def = r#"{ "$liasse": 1, "$app": "t.st@1.0.0", "$model":
        { "items": { "$key": "id", "id": "text",
                     "slug": { "$type": "text", "$unique": true } } } }"#;
    let built = build(def);
    let model = built.expect_ok();
    let items = match &model.root().member("items").expect("items").node {
        liasse_model::Node::Collection(collection) => collection,
        other => panic!("items is not a collection: {other:?}"),
    };
    assert!(
        items.unique.iter().any(|k| k.iter().any(|n| n.as_str() == "slug")),
        "key-eligible `$unique: true` shorthand did not register a candidate key"
    );
}

#[test]
fn param_conflicting_key_types_rejected() {
    // §8.3: "All uses of the same parameter MUST infer one compatible type."
    // `@x` keys `tasks` (a `text` key, so `@x: text`) and also keys `items` (an
    // `int` key, so `@x: int`). `text` and `int` are not one compatible type, so
    // the mutation has no single parameter contract and the package MUST be
    // rejected.
    let def = r#"{ "$liasse": 1, "$app": "t.conflict@1.0.0", "$model": {
        "tasks": { "$key": "id", "id": "text", "done": "bool = false" },
        "items": { "$key": "id", "id": "int", "active": "bool = false" },
        "$mut": { "m": [".tasks[@x].done = true", ".items[@x].active = true"] }
    } }"#;
    let built = build(def);
    assert!(
        built.result.is_err(),
        "parameter used as two incompatible collection keys accepted with one arbitrary type"
    );
}

#[test]
fn param_consistent_key_types_control_accepted() {
    // Control: the same parameter keying two `text`-keyed collections infers one
    // compatible type and loads, proving the rejection above is about the
    // type conflict, not about using one parameter across two selectors.
    let def = r#"{ "$liasse": 1, "$app": "t.consistent@1.0.0", "$model": {
        "tasks": { "$key": "id", "id": "text", "done": "bool = false" },
        "items": { "$key": "id", "id": "text", "active": "bool = false" },
        "$mut": { "m": [".tasks[@x].done = true", ".items[@x].active = true"] }
    } }"#;
    let built = build(def);
    built.expect_ok();
}
