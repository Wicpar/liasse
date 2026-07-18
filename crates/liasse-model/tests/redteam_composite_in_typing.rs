//! Red-team regression: membership (`object in composite-keyed-view`) must be
//! validated at load like `==` is. Before the fix, `check_in` never applied the
//! composite-key coercion, so an object needle whose component type or arity did
//! not match the view's `$key` type-checked and silently evaluated FALSE — a
//! `$check`/`assert` the author believes constrains state became a no-op.
//!
//! Spec chain (all normative):
//!   * §6 intro: an expression's "static type ... is checked when the package is
//!     loaded". A checked type mismatch is a load-time rejection.
//!   * §6.3: a membership/equality against a keyed target compares "a key of the
//!     same declared target"; the supplied operand must be a key of THAT type.
//!   * A.9: a named object selector is authoring syntax for the same typed tuple
//!     in `$key` order, carrying the target key's component types. An `int` where
//!     a `text` component is declared, or a 1-tuple where a 2-tuple is declared,
//!     is not a key of the target.
//!
//! The membership operand shares the ONE validate-and-normalize point with the
//! `[{..}]` selector and `==` (`Checker::coerce_composite_key`), so the mismatched
//! cases below MUST reject exactly as the `==` analogue does.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// THE BUG (§6.3, A.9, §6 intro). An object needle whose `code` component is
/// `int` where the composite target key's `code` is `text`. MUST reject at load,
/// exactly as the `==` analogue does.
#[test]
fn composite_membership_with_mismatched_component_type_must_reject() {
    let def = r#"{ "$liasse": 1, "$app": "t.cin@1.0.0", "$model": {
        "regions": { "$key": ["region","code"], "region":"text", "code":"text" },
        "accounts": { "$key": "id", "id": "text" },
        "$mut": {
            "probe": "assert({ region: 'x', code: 5 } in .regions, 'must exist')"
        }
    } }"#;
    let built = build(def);
    assert!(
        built.result.is_err(),
        "§6.3/A.9/§6-intro: `{{ region:'x', code: 5 }} in .regions` supplies `code: 5` \
         (int) where the target key component `code` is `text`; a static type mismatch \
         checked at load. The model accepted it."
    );
}

/// THE BUG, arity variant (§6.3, A.9). A 1-component object needle where the
/// target key is a 2-tuple is not a key of the target — MUST reject.
#[test]
fn composite_membership_with_wrong_arity_must_reject() {
    let def = r#"{ "$liasse": 1, "$app": "t.cin@1.0.0", "$model": {
        "regions": { "$key": ["region","code"], "region":"text", "code":"text" },
        "accounts": { "$key": "id", "id": "text" },
        "$mut": {
            "probe": "assert({ region: 'x' } in .regions, 'must exist')"
        }
    } }"#;
    let built = build(def);
    assert!(
        built.result.is_err(),
        "§6.3/A.9: a 1-component object needle against a 2-tuple composite key is \
         not a key of the target and MUST reject at load. The model accepted it."
    );
}

/// Control: the correctly-typed composite membership loads, so the two rejections
/// above isolate the type/arity check, not membership over a composite view.
#[test]
fn control_composite_membership_with_correct_types_loads() {
    let def = r#"{ "$liasse": 1, "$app": "t.cin@1.0.0", "$model": {
        "regions": { "$key": ["region","code"], "region":"text", "code":"text" },
        "accounts": { "$key": "id", "id": "text" },
        "$mut": {
            "probe": "assert({ region: 'x', code: 'y' } in .regions, 'must exist')"
        }
    } }"#;
    build(def).expect_ok();
}
