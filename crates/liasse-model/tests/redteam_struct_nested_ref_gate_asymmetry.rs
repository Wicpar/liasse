//! RED-TEAM control (WAVE 2) for the struct-nested-ref runtime blindness finding
//! (`liasse-testkit/tests/redteam_struct_nested_ref_blind.rs`).
//!
//! This file proves the MODEL half of the model/runtime ASYMMETRY: the CORE
//! static model's §21.1 deferred-delete gate DOES descend into static structs
//! (`liasse-model/src/delete.rs::collect_refs`, the `Node::Struct` arm), so:
//!
//!   * a struct-nested `$ref` that omits `$on_delete` while a mutation can delete
//!     the target is REJECTED at load — exactly like a top-level ref (§21.1: "every
//!     inbound ref MUST declare one of restrict/cascade/none/= patch ... rejected as
//!     a whole when any inbound ref remains undecided"); and
//!   * a struct-nested `$ref` that DECLARES `$on_delete: cascade` LOADS cleanly.
//!
//! The developer is therefore FORCED to declare a delete policy on a struct-nested
//! ref and told the package is valid — while the runtime cascade planner
//! (`liasse-runtime/src/cascade.rs::plan`) silently ignores that policy (the FAILING
//! runtime tests). The static gate accepting the policy is precisely what makes the
//! runtime omission a latent-integrity bug rather than a rejected program.
//!
//! These are PASSING controls (the model behaves correctly); they exist to pin the
//! asymmetry so the runtime finding cannot be dismissed as "struct-nested refs are
//! unsupported". Expectations are derived from SPEC.md §21.1 / §5.6 text.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use liasse_diag::SourceMap;
use liasse_model::Model;
use liasse_syntax::parse_document;

/// Build a strict-JSON package definition and report whether the model accepts it.
fn loads(text: &str) -> bool {
    let mut sources = SourceMap::new();
    let id = sources.add_file("package.liasse", text);
    match parse_document(id, text) {
        Err(_) => false,
        Ok(doc) => Model::build(&mut sources, id, &doc).is_ok(),
    }
}

// The §21.1 gate DOES see a struct-nested undecided ref against a deletable
// target -> the package is rejected at load.
#[test]
fn model_gate_rejects_struct_nested_undecided_ref() {
    let text = r#"{
      "$liasse": 1,
      "$app": "t.gate.snu@1.0.0",
      "$model": {
        "accounts": { "$key": "id", "id": "text" },
        "tasks": {
          "$key": "id", "id": "text",
          "meta": { "owner": { "$ref": "/accounts" } }
        },
        "$mut": { "del": ".accounts - @id" }
      }
    }"#;
    assert!(
        !loads(text),
        "§21.1: a struct-nested undecided ref against a deletable target must be rejected at load"
    );
}

// Control: the same undecided ref at TOP level is likewise rejected (the gate is
// symmetric across nesting — as it must be).
#[test]
fn model_gate_rejects_toplevel_undecided_ref() {
    let text = r#"{
      "$liasse": 1,
      "$app": "t.gate.tlu@1.0.0",
      "$model": {
        "accounts": { "$key": "id", "id": "text" },
        "tasks": {
          "$key": "id", "id": "text",
          "owner": { "$ref": "/accounts" }
        },
        "$mut": { "del": ".accounts - @id" }
      }
    }"#;
    assert!(!loads(text), "§21.1: a top-level undecided ref against a deletable target must be rejected");
}

// The model ACCEPTS a struct-nested ref that declares `$on_delete: cascade`. This
// is the policy the runtime then ignores (see the runtime finding).
#[test]
fn model_accepts_struct_nested_cascade_policy() {
    let text = r#"{
      "$liasse": 1,
      "$app": "t.gate.snc@1.0.0",
      "$model": {
        "accounts": { "$key": "id", "id": "text" },
        "tasks": {
          "$key": "id", "id": "text",
          "meta": { "owner": { "$ref": "/accounts", "$on_delete": "cascade" } }
        },
        "$mut": { "del": ".accounts - @id" }
      }
    }"#;
    assert!(
        loads(text),
        "the model accepts a declared `$on_delete: cascade` on a struct-nested ref (which the runtime then ignores)"
    );
}
