//! §5.1/§5.2: a model-root computed value carries the type of its expression, so
//! a reference to it (`.name`) resolves to that type rather than the widest
//! `json` placeholder the field is built with. These lock the inference pass
//! [`infer`](../src/infer.rs): the expectations are re-derived from the spec text
//! (a computed value's type is its expression's type; §7.4 requires a `? :`
//! condition to be `bool`), not from the implementation's own output.

// Tests are expected to panic on a failed assertion (AGENTS.md).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// §5.2 + §7.4: a root computed `bool` value used as a view combinator condition
/// loads. Before the type was inferred the reference typed as `json`, which the
/// §7.4 `? :` condition check rejected as "condition must be `bool`".
#[test]
fn computed_bool_is_a_valid_view_condition() {
    build(
        r#"{
          "$liasse": 1
          "$app": "t.computed.cond@1.0.0"
          "$model": {
            "visible": "= 2 > 3"
            "tasks": { "$key": "id", "id": "text" }
            "$public": { "maybe": { "$view": ".visible ? .tasks { id } : []" } }
          }
        }"#,
    )
    .expect_ok();
}

/// The inference propagates across a sibling dependency: `shown` reads `visible`,
/// so `.shown` must resolve to `bool` too (a fixpoint over the two computed
/// values). Its use as a condition then type-checks.
#[test]
fn computed_bool_dependent_on_sibling_is_bool() {
    build(
        r#"{
          "$liasse": 1
          "$app": "t.computed.dep@1.0.0"
          "$model": {
            "visible": "= 2 > 3"
            "shown": "= .visible"
            "tasks": { "$key": "id", "id": "text" }
            "$public": { "maybe": { "$view": ".shown ? .tasks { id } : []" } }
          }
        }"#,
    )
    .expect_ok();
}

/// The inferred type is precise, not merely "anything": a root computed `text`
/// value is not a `bool`, so using it as a `? :` condition is still rejected
/// (§7.4). This distinguishes a genuine inference from blanket acceptance.
#[test]
fn computed_text_is_not_a_valid_view_condition() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.computed.text@1.0.0"
          "$model": {
            "label": "= string.trim(\" hi \")"
            "tasks": { "$key": "id", "id": "text" }
            "$public": { "maybe": { "$view": ".label ? .tasks { id } : []" } }
          }
        }"#,
    );
    built.expect_err();
    assert!(
        built.points_at(".label ? .tasks { id } : []"),
        "the rejection should point at the non-`bool` condition, got: {:?}",
        built.primary_spans()
    );
}
