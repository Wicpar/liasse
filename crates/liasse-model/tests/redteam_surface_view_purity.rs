//! Red-team: effect-class enforcement in the surface `$view` position (§8.8/§16.3).
//!
//! SPEC.md "Expression effects" (§8.8, line 1145): "Computed fields, views,
//! `$normalize`, and `$check` use pure functions only." §16.3 classifies `now()`
//! and `uuid()` as `generated` (non-deterministic) functions that "run in
//! mutation/write-time positions", never in a view — a view is cached,
//! materialized, and incrementally maintained (§7.1), so a generated call inside
//! it is unreproducible. "The checker rejects an effect class used in the wrong
//! position while loading the package" (§8.8, line 1147).
//!
//! A surface `$view` (§10.1: "`$view` defines its read result") is a view, so the
//! rule applies to it identically to a state-tree `$view`. These two cases feed
//! the byte-identical projection `.tasks { id, checked_at: now() }` — one as a
//! model-root `$view` declaration, one as a `$public` surface `$view` — so the
//! only difference is the declaration position.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// Positive control (§8.8): the identical `now()`-bearing projection declared as a
/// model-root `$view` is rejected as an effect-class violation. This confirms the
/// rule is real and enforced for the state-tree view position.
#[test]
fn state_tree_view_with_now_is_rejected() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "redteam.surfaceview@1.0.0"
          "$model": {
            "tasks": {
              "$key": "id"
              "id": "uuid = uuid()"
              "done": "bool = false"
            }
            "stamped": { "$view": ".tasks { id, checked_at: now() }" }
          }
        }"#,
    );
    // §8.8: a view uses pure functions only; `now()` is generated (§16.3).
    assert!(
        built.result.is_err(),
        "a model-root `$view` calling now() must be rejected (§8.8)"
    );
    assert!(
        built.has_code("M-EXPR"),
        "expected the pure-position diagnostic, got: {}",
        built.rendered()
    );
}

/// The bug (§8.8): a `$public` surface `$view` calling the generated `now()` MUST
/// be rejected exactly as the state-tree view above is — a surface `$view` "defines
/// its read result" (§10.1) and is therefore a view (§8.8). The effect-class
/// checker must reject the generated call in this pure position while loading.
#[test]
fn public_surface_view_with_now_must_be_rejected() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "redteam.surfaceview@1.0.0"
          "$model": {
            "tasks": {
              "$key": "id"
              "id": "uuid = uuid()"
              "done": "bool = false"
            }
            "$public": {
              "stamped": { "$view": ".tasks { id, checked_at: now() }" }
            }
          }
        }"#,
    );
    // §8.8/§16.3: the generated `now()` may not appear in a view position.
    assert!(
        built.result.is_err(),
        "a $public surface `$view` calling the generated now() must be rejected \
         as an effect-class violation (§8.8/§16.3), but the package loaded"
    );
    assert!(
        built.has_code("M-EXPR"),
        "expected the pure-position diagnostic (M-EXPR), got: {}",
        built.rendered()
    );
}
