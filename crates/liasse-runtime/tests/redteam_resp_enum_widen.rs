#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Annex E.5/E.7 probe: an exhaustive enum *result* exposed only through a
//! mutation response.
//!
//! E.5 lists "widening an exhaustively declared enum result" as a breaking output
//! change, and E.7 says "narrowing the declared response is breaking". A minor or
//! patch that widens such a result MUST be rejected before activation (E.1,
//! §13.14, §20.3). The existing corpus proves the runtime enforces this for a
//! *view* output (`minor_widens_enum_result_label_rejected`); this case isolates
//! the same exhaustive enum onto a mutation *response* member instead, where the
//! contract check compares only response member *names*.

mod support;

use liasse_runtime::{RejectionReason, UpdateError};
use support::{generator, load};

// The exhaustive enum `status` is exposed ONLY through the mutation response
// `. { id, status }`. The public view projects `{ id, seen }` (never `status`),
// and the `mark` mutation writes `seen` (never taking or writing `status`), so
// the enum touches neither the view-output side nor the input side of the
// boundary contract — only the mutation response promises it.
const V1: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.respenum@1.0.0"
  "$model": {
    "tickets": {
      "$key": "id",
      "id": "text",
      "seen": "bool = false",
      "status": { "$enum": ["draft", "active", "closed"] },
      "$mut": { "mark": [".seen = true", "return . { id, status }"] }
    }
    "$public": {
      "tickets": {
        "$view": ".tickets { id, seen }",
        "$mut": { "mark": ".tickets[@id].mark()" }
      }
    }
  }
  "$data": { "tickets": { "t1": { "status": "draft" } } }
}"#;

#[test]
fn minor_widens_enum_response_result_rejected() {
    let mut engine = load("respenum", V1);
    // v1.1 widens the exhaustive `status` enum result with `archived`. A client
    // that exhaustively matched {draft, active, closed} on the `mark` response
    // can now receive `archived`, so E.5 makes this a breaking output change.
    let target = V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#"["draft", "active", "closed"]"#, r#"["draft", "active", "closed", "archived"]"#);

    let mut generator = generator();
    match engine.update(&target, &mut generator) {
        Err(UpdateError::Rejected(rejection)) => {
            assert_eq!(
                rejection.reason(),
                RejectionReason::Compatibility,
                "widening an enum result is a compatibility narrowing (E.5): {}",
                rejection.message()
            );
        }
        other => panic!(
            "E.5 requires rejecting a widened exhaustive enum result on a minor release; \
             the engine instead admitted it: {other:?}"
        ),
    }
}

// A second, independent shape of the same gap: E.5 "making a required output
// optional" is breaking. `label` is a required (`text`) response member exposed
// only through the mutation response; v1.1 makes it `text?`. The response check
// compares member *names* only, so the narrowing is not detected.
const REQOPT_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.respopt@1.0.0"
  "$model": {
    "tickets": {
      "$key": "id",
      "id": "text",
      "seen": "bool = false",
      "label": "text",
      "$mut": { "mark": [".seen = true", "return . { id, label }"] }
    }
    "$public": {
      "tickets": {
        "$view": ".tickets { id, seen }",
        "$mut": { "mark": ".tickets[@id].mark()" }
      }
    }
  }
  "$data": { "tickets": { "t1": { "label": "L" } } }
}"#;

#[test]
fn minor_makes_required_response_member_optional_rejected() {
    let mut engine = load("respopt", REQOPT_V1);
    let target = REQOPT_V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""label": "text","#, r#""label": "text?","#);

    let mut generator = generator();
    match engine.update(&target, &mut generator) {
        Err(UpdateError::Rejected(rejection)) => {
            assert_eq!(rejection.reason(), RejectionReason::Compatibility);
        }
        other => panic!(
            "E.5 requires rejecting a required response member made optional on a minor \
             release; the engine instead admitted it: {other:?}"
        ),
    }
}

// Positive control: an UNCHANGED response commits, and an ADDITIVE widening (a new
// optional response member) commits — the typed comparison must not over-reject.
#[test]
fn response_unchanged_and_additive_commit() {
    // Unchanged response (with an additive optional field on the collection):
    // identical `status` domain across the minor still commits.
    let mut engine = load("respsame", V1);
    let unchanged = V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""seen": "bool = false","#, r#""seen": "bool = false", "note": "text?","#);
    let mut gen1 = generator();
    engine
        .update(&unchanged, &mut gen1)
        .expect("an unchanged response is compatible and commits");

    // Additive response member: v1.1 adds an optional `note` to the response.
    let mut engine2 = load("respadd", V1);
    let additive = V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""seen": "bool = false","#, r#""seen": "bool = false", "note": "text?","#)
        .replace(r#""return . { id, status }""#, r#""return . { id, status, note }""#);
    let mut gen2 = generator();
    engine2
        .update(&additive, &mut gen2)
        .expect("adding an optional response member is compatible and commits");
}
