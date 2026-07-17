//! Red-team regression: a `$ref` field is a valid `$unique` candidate-key
//! component, but the model builder rejects it.
//!
//! Spec chain (all normative):
//!   * §10.3 shows `members: { $key: ["company", "account"], company:
//!     { $ref: "/companies" }, account: { $ref: "/accounts" } }` — a `$ref`
//!     field IS a valid *primary* key component.
//!   * A.9: "`ref<T>` has the exact key type of its target collection or keyed
//!     view." A required ref therefore contributes its target's (eligible) base
//!     key type wherever it is used as a key component.
//!   * A.8: "Candidate-key components use the same eligible base types" as
//!     primary keys; §5.7: "Every present component MUST have a key-eligible
//!     type."
//!
//! Since a required ref is a valid primary-`$key` component (its effective type
//! is the target's eligible base key type per A.9), and A.8 says candidate-key
//! components use *the same eligible base types*, a required ref is equally a
//! valid `$unique` candidate-key component. The builder accepts the ref for
//! `$key` (control below) but rejects it for `$unique`, contradicting A.8's
//! "same eligible base types" — it statically rejects a package the spec
//! requires to load.
//!
//! Root cause: `crate::build::keys::Builder::unique_field` requires the
//! candidate-key member to be a `Node::Scalar` ("candidate-key field `{name}`
//! must be a scalar field"), whereas `key_field_type` special-cases a required
//! `Node::Reference` as valid. The `$unique` path has no such ref case.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// The bug: a single-field `$unique` naming a required `$ref` field is rejected,
/// though §5.7 + A.8 + A.9 + §10.3 make it a valid candidate key.
#[test]
fn ref_field_is_a_valid_unique_candidate_key() {
    let def = r#"{ "$liasse": 1, "$app": "t.u@1.0.0", "$model": {
        "accounts": { "$key": "id", "id": "text" },
        "companies": { "$key": "id", "id": "text" },
        "members": {
            "$key": ["company", "account"],
            "$unique": ["account"],
            "company": { "$ref": "/companies" },
            "account": { "$ref": "/accounts" }
        }
    } }"#;
    let built = build(def);
    assert!(
        built.result.is_ok(),
        "a required `$ref` field is a valid `$unique` candidate-key component \
         (§5.7, A.8 'same eligible base types', A.9, §10.3), but the builder \
         rejected the package:\n{}",
        built.rendered()
    );
}

/// Control: the SAME required ref used as a *primary* `$key` component loads.
/// This proves the implementation itself treats a required ref as a valid key
/// component, so the rejection above is specific to the `$unique` path — the
/// divergence is `$unique`-vs-`$key` inconsistency, not a mis-derived
/// expectation about refs.
#[test]
fn control_ref_field_is_a_valid_primary_key_component() {
    let def = r#"{ "$liasse": 1, "$app": "t.u@1.0.0", "$model": {
        "accounts": { "$key": "id", "id": "text" },
        "members": {
            "$key": "account",
            "account": { "$ref": "/accounts" },
            "admin": "bool = false"
        }
    } }"#;
    build(def).expect_ok();
}

/// Control: a scalar `$unique` candidate key loads, proving the `$unique`
/// machinery works and the rejection above is caused by the component being a
/// ref, not by `$unique` itself.
#[test]
fn control_scalar_unique_candidate_key_loads() {
    let def = r#"{ "$liasse": 1, "$app": "t.u@1.0.0", "$model": {
        "members": {
            "$key": "id",
            "$unique": ["email"],
            "id": "text",
            "email": "text"
        }
    } }"#;
    build(def).expect_ok();
}

/// Boundary: an *optional* `$ref` is a valid `$unique` candidate-key component.
/// A.8 pins that "the candidate-key fields themselves MAY be optional" — a rule
/// on candidate keys, distinct from the row-key exclusion of optionals — and the
/// corpus note `annex-a-types-wire/common/optional-type-excluded-from-row-key`
/// spells out that this candidate-key allowance does not extend to row keys. So,
/// unlike a primary `$key`, an optional ref MUST be accepted here (the row just
/// does not participate while the ref is `none`). This is the one intentional
/// asymmetry with the primary-key path.
#[test]
fn optional_ref_field_is_a_valid_unique_candidate_key() {
    let def = r#"{ "$liasse": 1, "$app": "t.u@1.0.0", "$model": {
        "accounts": { "$key": "id", "id": "text" },
        "members": {
            "$key": "id",
            "$unique": ["owner"],
            "id": "text",
            "owner": { "$ref": "/accounts", "$optional": true }
        }
    } }"#;
    build(def).expect_ok();
}

/// Boundary: a genuinely non-key-eligible member (a `$set` field) is still
/// rejected as a candidate key (§5.7 "key-eligible type", A.8 excludes sets).
/// This proves the fix did not broaden `$unique` beyond the scalar/ref parity.
#[test]
fn control_non_key_eligible_unique_member_still_rejected() {
    let def = r#"{ "$liasse": 1, "$app": "t.u@1.0.0", "$model": {
        "members": {
            "$key": "id",
            "$unique": ["tags"],
            "id": "text",
            "tags": { "$set": "text" }
        }
    } }"#;
    let built = build(def);
    assert!(
        built.result.is_err(),
        "a `$set` field is not a key-eligible candidate-key component (§5.7, A.8), \
         but the package loaded"
    );
}
