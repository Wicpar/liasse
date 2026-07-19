//! RED-TEAM (§10.5 recursive surface coverage — LOAD-TIME static checker).
//!
//! §10.5 (SPEC.md ~1484): "The output appears under `$field` as a nested keyed
//! view — a keyed tree in which every node's ancestors are all included. **The
//! checker verifies descendant shape, acyclicity, identity, and predicate
//! types.**" And admission addresses a covered descendant by "the descendant's
//! key path from that row down through `$field`/`$through`" (§10.5 line 1486,
//! `tests/10-interfaces-roles/NOTES.md` line 47) — `$field` and `$through` name
//! ONE descendant relation.
//!
//! The runtime *materialization* of coverage is acknowledged debt in the
//! scenario SKIP ledger ("recursive coverage validated but not materialized at
//! runtime"), so it is off-limits here. This file attacks the LOAD-TIME checker
//! (`liasse-model`'s `check_recursive`, `crates/liasse-model/src/surface.rs`),
//! which runs on every load and is NOT gated by that ledger.
//!
//! Two obligations §10.5 places on the checker are unmet:
//!
//!  1. `$field` must name a KEYED COLLECTION of the descendant shape — the nested
//!     keyed view lives "under `$field`" and its nodes have identity. The checker
//!     (`surface.rs` ~line 314) only verifies the field EXISTS, so a SCALAR
//!     `$field` (or any non-collection field) is wrongly accepted: nothing can
//!     hold a keyed tree, and "identity"/"descendant shape" are unverifiable.
//!  2. `$field` and `$through` must name the SAME descendant relation. The checker
//!     validates each INDEPENDENTLY (`$field` exists; `$through` yields keyed
//!     descendants) and never that `$through` descends into `$field`, so a
//!     divergent pair is accepted — the nested view placed under `$field` then
//!     carries rows of a DIFFERENT declared shape than `$field`, and the
//!     `$field`/`$through` key-path addressing is incoherent.
//!
//! Each bug-repro asserts the spec-mandated REJECTION; it FAILS today because the
//! checker LOADS the malformed coverage. The controls PASS, isolating the one
//! variable under test (the surrounding role/auth/self-referential shape is the
//! exact valid corpus shape from `recursive-coverage-nests-included-descendants`).
//!
//! NOT reported here (deliberately excluded as already-tracked / entangled):
//!  - runtime non-materialization of coverage — SKIP-ledger acknowledged debt;
//!  - a role `$view` projecting a field the (recursive) descendant row lacks —
//!    entangled with the acknowledged role-`$view` full-typing seam
//!    (SPEC-ISSUES #10: a role `$view` is purity/param-checked but not fully
//!    typed for the `$actor` seam), not a §10.5-specific checker gap;
//!  - `$through` revisiting a row through a `.`-rooted *ref* deref — a documented
//!    residual of the structural strict-descendant check (`surface.rs` module docs).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;
use common::build;

/// The valid coverage model with `$view` and `$recursive` injected. Everything
/// but those two placeholders is the exact shape of the passing corpus case
/// `recursive-coverage-nests-included-descendants`.
const TEMPLATE: &str = r#"{
  "$liasse": 1,
  "$app": "t.rcfield@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text" },
    "companies": {
      "$key": "id",
      "id": "text",
      "name": "text",
      "plan": "text = 'active'",
      "subcompanies": { "$like": "^" },
      "members": {
        "$key": "account",
        "account": { "$ref": "/accounts" },
        "admin": "bool = false"
      },
      "$roles": {
        "admin": {
          "$auth": "token",
          "$members": ".members[:m | m.admin].account",
          "company": {
            "$view": "__VIEW__",
            "$recursive": __RECURSIVE__
          }
        }
      }
    },
    "$auth": {
      "token": { "$credential": "text", "$verify": "$credential", "$actor": "/accounts[$proof]" }
    }
  }
}"#;

fn model(view: &str, recursive: &str) -> common::Built {
    build(&TEMPLATE.replace("__VIEW__", view).replace("__RECURSIVE__", recursive))
}

// ===========================================================================
// CONTROLS — these PASS, proving the harness and the surrounding shape are
// sound and that the checker's `$recursive` validation genuinely runs, so a
// failure of a bug-repro below is the isolated gap and not incidental noise.
// ===========================================================================

/// The exact valid corpus coverage (`$field: subcompanies`, `$through:
/// .subcompanies`, both naming the self-referential keyed collection) loads.
#[test]
fn control_valid_coverage_loads() {
    let built = model(
        ". { id, name, plan }",
        r#"{ "$field": "subcompanies", "$through": ".subcompanies", "$bind": "child" }"#,
    );
    built.expect_ok();
}

/// A `$field` that names NO field of the covered row is rejected — the field
/// existence check runs. This isolates the bug in `scalar_field_...`: the check
/// is present but only tests existence, never collection/keyed-ness.
#[test]
fn control_absent_field_is_rejected() {
    let built = model(
        ". { id, name, plan }",
        r#"{ "$field": "nope", "$through": ".subcompanies", "$bind": "child" }"#,
    );
    assert!(built.result.is_err(), "an absent `$recursive` `$field` must be rejected (§10.5)");
    assert!(built.has_code(liasse_model::code::SURFACE), "the rejection is a surface diagnostic: {}", built.rendered());
}

/// A `$through` that does not yield a row stream at all (a scalar traversal) is
/// rejected — the `$through`-yields-keyed-descendants check runs.
#[test]
fn control_scalar_through_is_rejected() {
    let built = model(
        ". { id, name }",
        r#"{ "$field": "subcompanies", "$through": ".name", "$bind": "child" }"#,
    );
    assert!(built.result.is_err(), "a `$through` that is not a descendant row stream must be rejected (§10.5)");
}

// ===========================================================================
// BUG REPROS — these FAIL today: the checker LOADS the malformed coverage the
// §10.5 obligations require it to reject. A failing assertion here IS the bug.
// ===========================================================================

/// FINDING 1 (§10.5 descendant shape + identity). `$field: "name"` names a
/// SCALAR `text` field. §10.5: "The output appears under `$field` as a nested
/// keyed view — a keyed tree in which every node's ancestors are all included.
/// The checker verifies descendant shape, acyclicity, identity ...". A scalar
/// field cannot hold a keyed tree and has no row identity, so the coverage can
/// never materialize under it. The checker MUST reject a non-collection `$field`,
/// but it only verifies the field exists (`surface.rs` ~line 314) and so accepts
/// this — a genuine, un-materializable coverage admitted at load.
#[test]
fn scalar_field_must_be_rejected() {
    let built = model(
        ". { id, name, plan }",
        r#"{ "$field": "name", "$through": ".subcompanies", "$bind": "child" }"#,
    );
    assert!(
        built.result.is_err(),
        "SPEC VIOLATION (§10.5): `$recursive` `$field` names the SCALAR field `name`, but the \
         coverage output must be a nested KEYED view under `$field` (a keyed tree whose nodes have \
         identity). A scalar field can hold no such tree, yet the checker accepted the package. \
         `check_recursive` (crates/liasse-model/src/surface.rs ~L314) only verifies `$field` \
         EXISTS, never that it is a keyed collection of the descendant shape."
    );
}

/// FINDING 1, variant: `$field` names the `members` KEYED collection (so it *is*
/// keyed), but that collection's declared row shape (`{account, admin}`) is NOT
/// the descendant shape produced by `$through: .subcompanies` (`{id, name, plan,
/// subcompanies, members, ...}`). §10.5 nests the coverage tree "under `$field`",
/// so `$field`'s element shape must be the descendant shape. The checker never
/// compares them, so it accepts a nested view whose rows do not match the field
/// they are placed under.
#[test]
fn field_collection_shape_must_match_descendant() {
    let built = model(
        ". { id, name, plan }",
        r#"{ "$field": "members", "$through": ".subcompanies", "$bind": "child" }"#,
    );
    assert!(
        built.result.is_err(),
        "SPEC VIOLATION (§10.5): `$field` = `members` (row shape {{account, admin}}) but `$through` \
         = `.subcompanies` yields subcompany-shaped rows. The nested keyed view placed under \
         `$field` therefore carries rows of a DIFFERENT declared shape than `members`, violating \
         the §10.5 'checker verifies descendant shape'/identity obligation, yet the package loaded."
    );
}

/// FINDING 2 (§10.5 `$field`/`$through` coherence). `$field: "subcompanies"` but
/// `$through: ".members"` — the two name UNRELATED descendant relations. §10.5
/// addresses a covered descendant by its "key path ... down through
/// `$field`/`$through`" (line 1486; NOTES.md line 47), treating them as ONE
/// relation, and nests the output "under `$field`". When `$through` does not
/// descend into `$field`, that key-path addressing is incoherent (the path keys
/// index `subcompanies` while the traversal walks `members`) and the tree nested
/// under `subcompanies` is built from `members` rows. The checker validates
/// `$field` and `$through` INDEPENDENTLY and never that `$through` traverses
/// `$field`, so it accepts the divergent pair.
#[test]
fn field_and_through_must_name_same_relation() {
    let built = model(
        ". { id, name }",
        r#"{ "$field": "subcompanies", "$through": ".members", "$bind": "child" }"#,
    );
    assert!(
        built.result.is_err(),
        "SPEC VIOLATION (§10.5): `$field` = `subcompanies` but `$through` = `.members` names an \
         unrelated descendant relation. §10.5 addresses covered descendants 'down through \
         `$field`/`$through`' as ONE relation and nests the tree 'under `$field`', so `$through` \
         must descend into `$field`; the checker validates them independently and accepted the \
         divergent pair."
    );
}
