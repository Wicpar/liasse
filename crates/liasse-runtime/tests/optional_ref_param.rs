#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.3: a mutation parameter inferred from an `$optional` `$ref` field is an
//! OPTIONAL parameter, symmetric with an optional scalar field.
//!
//! An optional scalar field bakes its optionality into the field type
//! (`Type::Optional`), so a parameter inferred from it (`{ note: @note }`) is
//! optional and an omitted argument binds `none` (§8.3/§A.1). An optional `$ref`
//! field carried a required-by-default `ref` typing that dropped the field's
//! `$optional`, so a parameter inferred from it (`{ role: @role }`) was enforced
//! as REQUIRED — a call omitting it rejected with `Malformed: missing argument`.
//! This is the Bilani `stage_line` blocker: a direct-posting accounting line with
//! optional `@tiers`/`@role` refs could never be staged without an auxiliary.
//!
//! Every expectation is re-derived from SPEC.md §8.3 (optional parameters bind
//! `none`) and §5.6/§22.1 (ref integrity), not from the implementation.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, RejectionReason, Value};
use liasse_store::MemoryStore;
use liasse_value::{Ref, Text};
use support::{generator, load};

type Eng = Engine<MemoryStore>;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// A `$ref` value keyed by a scalar `text` row id.
fn role_ref(id: &str) -> Value {
    Value::Ref(Ref::scalar(text(id)))
}

/// A model whose `lines` rows carry an OPTIONAL `$ref` (`role`) alongside an
/// optional scalar control (`note`), plus a `bonds` collection with a genuinely
/// REQUIRED `$ref` (`owner`). Each mutation inserts exactly one of these fields
/// from a parameter, so the parameter's optionality is inferred from that one
/// field's declaration and nothing else.
const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.optref@1.0.0",
  "$model": {
    "roles": { "$key": "id", "id": "text", "label": "text" },
    "lines": {
      "$key": "id",
      "id": "text",
      "role": { "$ref": "/roles", "$optional": true },
      "note": "text?"
    },
    "bonds": { "$key": "id", "id": "text", "owner": { "$ref": "/roles" } },
    "lines_view": { "$view": ".lines { id, role, note }" },
    "$mut": {
      "add_role": ".roles + { id: @id, label: @label }",
      "stage_line": ".lines + { id: @id, role: @role }",
      "stage_note": ".lines + { id: @id, note: @note }",
      "add_bond": ".bonds + { id: @id, owner: @owner }"
    }
  }
}"#;

fn commit(outcome: CallOutcome) {
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "expected a commit, got {outcome:?}");
}

/// The single `lines` row after staging, or `None` if nothing was staged.
fn line_role(engine: &Eng) -> Option<Value> {
    let view = engine.view_at_head("lines_view").expect("view").expect("declared");
    view.rows()[0].field("role").cloned()
}

/// Seed one live role so a supplied ref resolves.
fn with_role(engine: &mut Eng) {
    let mut g = generator();
    commit(
        engine
            .call(&CallRequest::new("add_role").arg("id", text("r1")).arg("label", text("Client")), &mut g)
            .expect("call"),
    );
}

/// Test 1 — the fix. Omitting the optional-`$ref` argument ADMITS: the parameter
/// binds `none` (§8.3/§A.1) and the row commits with the ref field absent.
#[test]
fn omitted_optional_ref_param_admits_and_commits_row_with_field_absent() {
    let mut engine = load("optref1", M);
    let mut g = generator();
    let outcome = engine
        .call(&CallRequest::new("stage_line").arg("id", text("l1")), &mut g)
        .expect("the call reaches admission");
    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "omitting an optional-$ref argument must admit and commit, got {outcome:?}"
    );
    assert_eq!(line_role(&engine), None, "the optional $ref field reads back absent when its argument is omitted");
}

/// Test 2 — supplying the optional-`$ref` argument ADMITS and resolves the ref
/// exactly as before: the committed row carries the supplied reference.
#[test]
fn supplied_optional_ref_param_admits_and_resolves() {
    let mut engine = load("optref2", M);
    with_role(&mut engine);
    let mut g = generator();
    let outcome = engine
        .call(&CallRequest::new("stage_line").arg("id", text("l1")).arg("role", role_ref("r1")), &mut g)
        .expect("the call reaches admission");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "a supplied live ref commits, got {outcome:?}");
    assert_eq!(line_role(&engine), Some(role_ref("r1")), "the supplied optional $ref resolves and reads back");
}

/// Test 3 — control. An optional SCALAR parameter is unchanged: omitting it still
/// admits and commits with the field absent (the fix does not disturb scalars).
#[test]
fn control_omitted_optional_scalar_param_still_admits() {
    let mut engine = load("optref3", M);
    let mut g = generator();
    let outcome = engine
        .call(&CallRequest::new("stage_note").arg("id", text("l1")), &mut g)
        .expect("the call reaches admission");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "an omitted optional scalar still admits, got {outcome:?}");
    let view = engine.view_at_head("lines_view").expect("view").expect("declared");
    assert_eq!(view.rows()[0].field("note"), None, "the optional scalar field reads back absent when omitted");
}

/// Test 4 — control. A genuinely REQUIRED `$ref` parameter STILL rejects when
/// omitted: the fix must not over-relax required refs.
#[test]
fn control_omitted_required_ref_param_still_rejects() {
    let mut engine = load("optref4", M);
    let mut g = generator();
    let outcome = engine
        .call(&CallRequest::new("add_bond").arg("id", text("b1")), &mut g)
        .expect("the call reaches admission");
    assert_eq!(
        outcome.rejection().map(liasse_runtime::Rejection::reason),
        Some(RejectionReason::Malformed),
        "a required-$ref parameter is still required (§8.3), got {outcome:?}"
    );
}

/// Test 5 — control. Supplying an optional-`$ref` argument that points at a
/// non-existent target STILL fails ref integrity (§5.6/§22.1): optionality does
/// not bypass reference validation.
#[test]
fn control_supplied_optional_ref_to_missing_target_fails_integrity() {
    let mut engine = load("optref5", M);
    let mut g = generator();
    let outcome = engine
        .call(&CallRequest::new("stage_line").arg("id", text("l1")).arg("role", role_ref("ghost")), &mut g)
        .expect("the call reaches admission");
    assert_eq!(
        outcome.rejection().map(liasse_runtime::Rejection::reason),
        Some(RejectionReason::DanglingRef),
        "a supplied optional $ref must still resolve to a live row (§5.6/§22.1), got {outcome:?}"
    );
}
