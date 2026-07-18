#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team regression: the Annex E boundary-narrowing check compares a view's
//! exposed **row identity by its `$key` field NAMES only**, never the component
//! TYPES. A same-major minor that retypes one component of a **composite** key —
//! `code: text` → `code: int` — while keeping the field names is a change to the
//! exposed row identity (A.9 typed tuple), which E.3 mechanically compares and E.5
//! makes breaking. It is admitted as a compatible minor when the retyped component
//! is not itself a projected output member, so the identity path is the only thing
//! that could catch it.
//!
//! Spec chain (all normative, Annex E is normative):
//!   * A.9 (SPEC.md line 4479): "`ref<T>` has the exact key type of its target
//!     collection or keyed view. ... A composite key uses an array of component
//!     wire values in `$key` order". The composite key is a *typed tuple*;
//!     changing a component's type changes that tuple's type — the exposed
//!     identity's type.
//!   * E.3 (line 5192): mechanically decidable checks include "row identity ...";
//!     "types, optionality, defaults ..." — identity IS mechanically compared, and
//!     types are what the mechanical comparison is made of.
//!   * E.5 (line 5236): breaking output changes include "changing exposed row
//!     identity". Retyping an identity component from `text` to `int` changes it.
//!   * §20.3/E.9: `load` and update reject a narrowing release before activation;
//!     the current package stays active.
//!
//! The collection is **empty** (no `$data`), so a `text`→`int` component retype
//! passes prospective-state validation trivially (no stored row to invalidate) and
//! the ONLY thing that can reject the update is the Annex E narrowing check. If
//! that check misses it, the minor COMMITS — the bug. The controls below prove the
//! same checker (a) catches the identical retype when the component IS projected
//! (via output-member typing) and (b) catches a composite-key NAME change through
//! the identity path — so the acceptance is a genuine identity-TYPE hole, not a
//! mis-derived expectation.
//!
//! Root cause: `liasse-runtime/src/contract/mod.rs`. `Output.identity` is
//! `Option<Vec<String>>` (key field names) and `exposed_identity` returns
//! `collection.key.clone()` (names). `surface_narrowing` flags a change only when
//! the name vectors differ (`a != c`), so a component TYPE change with unchanged
//! names slips through.

mod support;

use liasse_runtime::{Engine, RejectionReason, UpdateError};
use liasse_store::MemoryStore;
use support::{generator, load};

/// Attempt an update, returning the raw result so a test can distinguish "the
/// minor committed" (the bug) from "rejected for a compatibility narrowing" (the
/// spec-required outcome) from "rejected for some other reason".
fn try_update(engine: &mut Engine<MemoryStore>, target: &str) -> Result<(), UpdateError> {
    let mut generator = generator();
    engine.update(target, &mut generator).map(|_| ())
}

/// Assert an update is refused specifically as an Annex E boundary-contract
/// narrowing (E.9) — not merely rejected for some incidental migration reason.
fn assert_narrowing(engine: &mut Engine<MemoryStore>, target: &str) -> String {
    match try_update(engine, target) {
        Err(UpdateError::Rejected(rejection)) if rejection.reason() == RejectionReason::Compatibility => {
            rejection.message().to_owned()
        }
        other => panic!("expected an Annex E compatibility narrowing rejection, got {other:?}"),
    }
}

// v1: a keyed collection with a **composite** key `["region", "code"]` whose
// `code` component is `text`. The public view projects only the NON-key field
// `rate`, so the composite identity `(region, code)` is exposed as the row
// identity (§7.2, inherited) but neither key component is an output member. Empty
// (no `$data`), so no stored row constrains a later component retype.
const V1: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.compid@1.0.0"
  "$model": {
    "rates": {
      "$key": ["region", "code"]
      "region": "text"
      "code": "text"
      "rate": "decimal"
    }
    "$public": { "rates": { "$view": ".rates { rate }" } }
  }
}"#;

/// THE BUG (A.9, E.3, E.5, §20.3). A minor retypes the composite key component
/// `code` from `text` to `int`. The exposed row identity's typed tuple changes
/// from `(text, text)` to `(text, int)` — a breaking identity change E.3
/// mechanically compares and E.5 forbids in a same-major forward release. With the
/// component unprojected and the collection empty, only the Annex E narrowing
/// check can reject it. It currently COMMITS: the identity comparison is over
/// `$key` field NAMES, which are unchanged.
#[test]
fn minor_retypes_composite_key_component_must_reject() {
    let mut engine = load("compid", V1);
    let target = V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""code": "text""#, r#""code": "int""#);
    // Spec-required: an Annex E compatibility narrowing rejection (E.5 exposed row
    // identity). If the engine instead commits (or the model even declines to
    // build), this panics — reproducing the composite-identity-TYPE hole.
    let message = assert_narrowing(&mut engine, &target);
    assert!(
        message.to_lowercase().contains("identity"),
        "the diagnostic must report an exposed-row-identity narrowing (E.5), got: {message}"
    );
    // E.9: on rejection the prior release stays active and still promises the
    // `(text, text)` composite identity.
    assert_eq!(engine.model().header().identity.version.minor, 0, "1.0.0 stays active");
}

// ---------------------------------------------------------------------------
// Controls: the SAME checker catches the retype when the component is projected
// (output-member typing) and catches a composite-key NAME change (identity path).
// Together they prove the acceptance above is a composite-identity-TYPE hole, not
// a mis-derived rule or a wholesale blind spot.
// ---------------------------------------------------------------------------

// v1 variant whose public view DOES project the composite key components, so the
// retype is visible to output-member typing.
const V1_PROJECTED: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.compidp@1.0.0"
  "$model": {
    "rates": {
      "$key": ["region", "code"]
      "region": "text"
      "code": "text"
      "rate": "decimal"
    }
    "$public": { "rates": { "$view": ".rates { region, code, rate }" } }
  }
}"#;

/// Control: the identical `code: text → int` retype, but now `code` is a projected
/// OUTPUT MEMBER. `output_narrows(text, int)` sees a changed value type, so the
/// checker rejects it via the output-member path. This proves the checker is not
/// blind to the component's type in general — only along the identity path.
#[test]
fn control_projected_composite_component_retype_rejected() {
    let mut engine = load("compidp", V1_PROJECTED);
    let target = V1_PROJECTED
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""code": "text""#, r#""code": "int""#);
    let message = assert_narrowing(&mut engine, &target);
    assert!(
        message.contains("output member") || message.contains("code"),
        "the projected retype is caught as an output-member narrowing: {message}"
    );
}

/// Control: a composite-key NAME change `["region","code"]` → `["region","zone"]`
/// (with the view still projecting only `rate`) IS caught through the identity
/// path, because the name vectors differ. This proves the identity comparison is
/// live for composite keys — it simply never inspects component types.
#[test]
fn control_composite_key_name_change_rejected() {
    let mut engine = load("compidn", V1);
    let target = V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#"["region", "code"]"#, r#"["region", "zone"]"#)
        .replace(r#""code": "text""#, r#""zone": "text""#);
    let message = assert_narrowing(&mut engine, &target);
    assert!(
        message.to_lowercase().contains("identity"),
        "a composite-key name change is an identity narrowing (E.5): {message}"
    );
}

/// Control (positive): a byte-identical model under a minor bump commits. Proves
/// the harness is sound and the checker does not reject a composite-keyed view
/// with an unprojected key wholesale — so the acceptance in the bug test is
/// specifically the missing identity-type comparison, not a dead code path.
#[test]
fn control_unchanged_composite_identity_commits() {
    let mut engine = load("compidok", V1);
    let target = V1.replace("@1.0.0", "@1.1.0");
    try_update(&mut engine, &target).expect("an unchanged composite-keyed minor commits");
    assert_eq!(engine.model().header().identity.version.minor, 1, "1.1.0 is active");
}

// ---------------------------------------------------------------------------
// Scalar analogue: the red-team flagged that a SCALAR `$key` retype with the key
// field unprojected is missed identically to the composite case — `Output.identity`
// carried field NAMES only, so a scalar `code: text → int` with the name unchanged
// slipped through. These two cases pin the scalar side of the same hole.
// ---------------------------------------------------------------------------

// v1 with a **scalar** `$key` `code: text`, projecting only the non-key `rate`, so
// the scalar identity `code` is exposed as the row identity (§7.2, inherited) but
// is not itself an output member. Empty, so no stored row constrains a later retype.
const V1_SCALAR: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.compids@1.0.0"
  "$model": {
    "rates": {
      "$key": "code"
      "code": "text"
      "rate": "decimal"
    }
    "$public": { "rates": { "$view": ".rates { rate }" } }
  }
}"#;

/// THE BUG (scalar analogue; A.9, E.3, E.5, §20.3). A minor retypes the SCALAR key
/// field `code` from `text` to `int`, the field name unchanged and `code`
/// unprojected. The exposed row identity's type changes from `text` to `int` — a
/// breaking identity change E.3 mechanically compares and E.5 forbids. With the
/// collection empty, only the Annex E narrowing check can reject it; before the fix
/// the identity comparison read the field NAME only, so this committed just like
/// the composite case.
#[test]
fn minor_retypes_scalar_key_field_must_reject() {
    let mut engine = load("compids", V1_SCALAR);
    let target = V1_SCALAR
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""code": "text""#, r#""code": "int""#);
    let message = assert_narrowing(&mut engine, &target);
    assert!(
        message.to_lowercase().contains("identity"),
        "the diagnostic must report an exposed-row-identity narrowing (E.5), got: {message}"
    );
    // E.9: on rejection the prior release stays active and still promises the
    // `text` scalar identity.
    assert_eq!(engine.model().header().identity.version.minor, 0, "1.0.0 stays active");
}

/// Control (scalar, positive): the same scalar-keyed model under a minor bump with
/// the key type unchanged commits — proving the scalar identity path does not
/// wholesale reject a scalar-keyed view with an unprojected key, so the rejection
/// above is specifically the missing component-type comparison.
#[test]
fn control_unchanged_scalar_identity_commits() {
    let mut engine = load("compidsok", V1_SCALAR);
    let target = V1_SCALAR.replace("@1.0.0", "@1.1.0");
    try_update(&mut engine, &target).expect("an unchanged scalar-keyed minor commits");
    assert_eq!(engine.model().header().identity.version.minor, 1, "1.1.0 is active");
}
