//! Red-team regression: the `==` equality checker enforces static type
//! agreement for scalar operands and scalar refs, but SILENTLY BYPASSES it for a
//! **composite** ref compared against an object literal — admitting both a
//! wrong-typed and a wrong-arity composite key where a scalar operand in the very
//! same position is a load-time type error.
//!
//! Spec chain (all normative):
//!   * §6 intro (SPEC.md line 634): expression "static type and effect class are
//!     checked when the package is loaded". A checked type mismatch is a
//!     load-time rejection, not a silently-false runtime.
//!   * §6.3 (line 704): "Equality between a row or ref and a key of the same
//!     declared target compares the current typed key" — the supplied operand
//!     must be a *key of that target's type*.
//!   * A.9: "A composite key uses an array of component wire values in `$key`
//!     order; named object selectors are authoring syntax for the same *typed
//!     tuple*." The tuple's components carry the target key's component types; an
//!     `int` where a `text` component is declared, or a 1-tuple where a 2-tuple
//!     is declared, is not a key of that target.
//!
//! The scalar CONTROLS below prove the checker itself treats a key-type mismatch
//! in `==` as a static error ("cannot compare `text` with `int`" / "cannot
//! compare `ref` with `int`"), so the composite acceptances are a genuine
//! composite-specific soundness hole, not a mis-derived expectation.
//!
//! Root cause: `liasse-expr/src/check/views.rs::Checker::coerce_composite_key`
//! re-types ANY `Struct`-shaped operand to the ref target's
//! `Type::Composite(components)` purely on the operand being a struct — it never
//! checks the struct's field names, types, or arity against the components. In
//! the `==` path (`check/ops.rs::coerce_composite_ref_key` -> `check_compare`),
//! `comparable()` then sees `Ref(Composite(X))` vs `Composite(X)` and returns
//! `true` via `ref_key_matches`, admitting the mismatched comparison. (The
//! *selector* arity gap, `06-expressions/composite-selector-missing-component-
//! invalid`, is separately ledgered in `corpus_static.rs`; this is the distinct,
//! UN-ledgered `==` compare path, and covers the wrong-TYPE case the arity ledger
//! entry does not name.)

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// THE BUG (§6.3 line 704, A.9, §6 intro). A composite ref compared against an
/// object literal whose `code` component is `int` where the target key's `code`
/// is `text`. A scalar `.name == 5` (text vs int) in the same `$check` position
/// is rejected (control below), so this MUST be rejected too. It currently loads.
#[test]
fn composite_ref_equality_with_mismatched_component_type_must_reject() {
    let def = r#"{ "$liasse": 1, "$app": "t.cmp@1.0.0", "$model": {
        "regions": { "$key": ["region","code"], "region":"text", "code":"text", "label":"text" },
        "items": {
            "$key": "id",
            "id": "text",
            "loc": { "$ref": "/regions" },
            "$check": ".loc == { region: 'x', code: 5 }"
        }
    } }"#;
    let built = build(def);
    assert!(
        built.result.is_err(),
        "§6.3/A.9/§6-intro: comparing a composite ref against an object literal \
         with a wrong-typed component (`code: 5` where the target key `code` is \
         `text`) is a static type mismatch and MUST be rejected at load, exactly \
         as the scalar analogue `.name == 5` is. The model accepted it:\n{}",
        built.expect_ok_debug()
    );
}

/// THE BUG, arity variant (§6.3, A.9). A composite ref compared against a
/// 1-component object literal where the target key is a 2-tuple. Distinct from
/// the ledgered *selector* arity case: this is the `==` compare path. Currently
/// loads; MUST reject (the operand is not a key of the target's type).
#[test]
fn composite_ref_equality_with_wrong_arity_must_reject() {
    let def = r#"{ "$liasse": 1, "$app": "t.cmp@1.0.0", "$model": {
        "regions": { "$key": ["region","code"], "region":"text", "code":"text", "label":"text" },
        "items": {
            "$key": "id",
            "id": "text",
            "loc": { "$ref": "/regions" },
            "$check": ".loc == { region: 'x' }"
        }
    } }"#;
    let built = build(def);
    assert!(
        built.result.is_err(),
        "§6.3/A.9: a composite ref (`$key: [region, code]`, a 2-tuple) compared \
         against a 1-component object literal is not a key of the target's type \
         and MUST be rejected at load. The model accepted it:\n{}",
        built.expect_ok_debug()
    );
}

// ------------------------------------------------------------------------
// Controls: the checker DOES enforce `==` key-type agreement elsewhere, so the
// two acceptances above are a composite-specific hole, not a mis-derived rule.
// ------------------------------------------------------------------------

/// Control: a plain scalar `==` type mismatch (`text` field vs `int` literal) in
/// the same `$check` position is a load-time type error. Establishes the scalar
/// baseline the composite path is supposed to match.
#[test]
fn control_scalar_equality_with_mismatched_type_is_rejected() {
    let def = r#"{ "$liasse": 1, "$app": "t.cmp@1.0.0", "$model": {
        "items": { "$key":"id", "id":"text", "name":"text", "$check": ".name == 5" }
    } }"#;
    let built = build(def);
    assert!(
        built.result.is_err(),
        "control: a scalar `text == int` comparison must be a static type error; \
         if this ever loads, the whole finding's baseline is void:\n{}",
        built.expect_ok_debug()
    );
}

/// Control: a SCALAR ref `==` type mismatch (`ref` vs `int`) is rejected. This is
/// the scalar-ref analogue of the composite-ref bug and shows the ref key-type is
/// checked when the key is scalar.
#[test]
fn control_scalar_ref_equality_with_mismatched_type_is_rejected() {
    let def = r#"{ "$liasse": 1, "$app": "t.cmp@1.0.0", "$model": {
        "accts": { "$key":"id", "id":"text" },
        "items": { "$key":"id", "id":"text", "owner": { "$ref":"/accts" },
                   "$check": ".owner == 5" }
    } }"#;
    let built = build(def);
    assert!(
        built.result.is_err(),
        "control: a scalar-ref `ref == int` comparison must be a static type \
         error:\n{}",
        built.expect_ok_debug()
    );
}

/// Control: the correctly-typed composite ref comparison loads, so the two BUG
/// tests are isolating the type/arity check, not rejecting composite `==` wholesale.
#[test]
fn control_composite_ref_equality_with_correct_types_loads() {
    let def = r#"{ "$liasse": 1, "$app": "t.cmp@1.0.0", "$model": {
        "regions": { "$key": ["region","code"], "region":"text", "code":"text", "label":"text" },
        "items": {
            "$key": "id",
            "id": "text",
            "loc": { "$ref": "/regions" },
            "$check": ".loc == { region: 'x', code: 'y' }"
        }
    } }"#;
    build(def).expect_ok();
}

// A tiny debug helper local to this test: render "(loaded)" when the model
// unexpectedly built, so a failing assertion prints something meaningful.
trait BuiltDebug {
    fn expect_ok_debug(&self) -> String;
}
impl BuiltDebug for common::Built {
    fn expect_ok_debug(&self) -> String {
        match &self.result {
            Ok(_) => "(model LOADED the package — expected a rejection)".to_owned(),
            Err(diags) => diags.render(&self.sources),
        }
    }
}
