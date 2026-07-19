//! RED TEAM — §5.2 computed values × §5.10 row checks, at §8.8 ADMISSION time. A
//! neighbour of the F3 landing (commit f252ed0): F3 taught `row_cell` to expose
//! static-struct MEMBERS to a prospective row `$check`; the same prospective row
//! still omits COMPUTED values, so a `$check` that reads one faults instead of
//! enforcing.
//!
//! # Finding (F-N2 — FIXED; these repros now run as regression guards)
//!
//! A row-level `$check` (§5.10) that reads a computed value (§5.2, `label: "=
//! .name"`; check `size(.label) > 0`) FAULTS at admission on every insert — good and
//! bad alike — with `Rejection{Evaluation}: "environment supplied a value that is
//! not a row with this field"`, making the collection entirely uninsertable. The
//! same computed value materializes correctly through a VIEW (read path), and a row
//! `$check` reading a stored FIELD instead of the computed enforces normally — so
//! the fault is specific to a `$check` reading a COMPUTED value at admission. It is
//! NOT self-ref specific: it reproduces on an ordinary flat collection.
//!
//! ## Root cause
//!
//! `check_row` (rules.rs:550-568) and `check_fields` (rules.rs:528) evaluate the
//! prospective `$check` over `row_cell(collection, fields)` (rules.rs:560). `row_cell`
//! (eval.rs:868-885) builds the prospective row from `collection.fields` and — after
//! F3 — `collection.structs`, but NOT `collection.computed`. So a check reading
//! `.label` (a computed value) hits a `Cell::Row` that has no `label` cell, and field
//! access raises `EvalError::ShapeMismatch { expected: "a row with this field" }`
//! (crates/liasse-expr/src/eval/mod.rs:262), surfaced as an `Evaluation` rejection.
//!
//! ## Why it is a bug
//!
//! §5.2 (SPEC.md:402) states a computed value "participates in views, **checks**,
//! sorting, and projections like any other value." A `$check` reading a computed
//! value is therefore spec-valid, and the model accepts it at load (it type-checks);
//! only admission faults. The fault is fail-closed (a false `Evaluation` refusal,
//! never a silent commit), so it is a correctness/usability gap, not an integrity or
//! authz breach. F-N2 fixes the ROOT: the checks now read the prospective row through
//! `EvalCtx::row_cell_of`, which folds the collection's computed values (derivable
//! from `fields`, in dependency order) onto the `row_cell` base before running checks,
//! exposing them the way the materialized read path already does.
//!
//! The `must_enforce` repros assert the SPEC-correct admission outcome (accept the
//! satisfying row, reject the violating one as a `Check`); with F-N2 landed they pass.
//! The controls fence the boundary. Every expectation is deducible from SPEC.md alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, RejectionReason, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// Run `add(id, name)` on a fresh copy of `definition` and return the outcome.
fn add(definition: &str, instance: &str, id: &str, name: &str) -> CallOutcome {
    let mut engine: Engine<MemoryStore> = load(instance, definition);
    let mut generator = generator();
    engine
        .call(&CallRequest::new("add").arg("id", text(id)).arg("name", text(name)), &mut generator)
        .expect("call dispatches (an admission refusal is an outcome, not an error)")
}

fn is_committed(o: &CallOutcome) -> bool {
    matches!(o, CallOutcome::Committed { .. })
}

fn is_check_rejection(o: &CallOutcome) -> bool {
    matches!(o, CallOutcome::Rejected(r) if r.reason() == RejectionReason::Check)
}

// ===========================================================================
// F-N2 REGRESSION GUARDS — assert the SPEC-correct admission outcome.
// Was RED before F-N2; GREEN now.
// ===========================================================================

/// A row `$check` reads the computed value `.label` (`= .name`) and requires it
/// non-empty. A row with a non-blank name MUST be admitted; a blank one MUST be a
/// `Check` rejection. Today both FAULT with an `Evaluation` "not a row with this
/// field" error (the prospective row omits the computed value).
const CHECK_READS_COMPUTED: &str = r##"{
  "$liasse": 1, "$app": "t.chkcomp@1.0.0",
  "$model": {
    "companies": { "$key": "id", "id": "text", "name": "text",
      "label": "= .name",
      "$check": ["size(.label) > 0", "label required"] },
    "$mut": { "add": ".companies + { id: @id, name: @name }" }
  }
}"##;

#[test]
fn row_check_reading_computed_must_enforce() {
    let good = add(CHECK_READS_COMPUTED, "chkcomp-good", "c1", "acme");
    assert!(is_committed(&good), "a row satisfying a computed-reading row check must be admitted, got {good:?}");
    let bad = add(CHECK_READS_COMPUTED, "chkcomp-bad", "c2", "");
    assert!(is_check_rejection(&bad), "a row violating a computed-reading row check must be a Check rejection, got {bad:?}");
}

/// The same finding on a SELF-REFERENTIAL collection (§5.8), where F1 already made
/// the computed value and the field-reading check compile. The computed-reading
/// check must still enforce at admission.
const SELFREF_CHECK_READS_COMPUTED: &str = r##"{
  "$liasse": 1, "$app": "t.srchkcomp@1.0.0",
  "$types": { "company": { "$key": "id", "id": "text", "name": "text",
    "label": "= .name",
    "$check": ["size(.label) > 0", "label required"],
    "subcompanies": "company" } },
  "$model": { "companies": "company",
    "$mut": { "add": ".companies + { id: @id, name: @name }" } }
}"##;

#[test]
fn selfref_row_check_reading_computed_must_enforce() {
    let good = add(SELFREF_CHECK_READS_COMPUTED, "srchkcomp-good", "c1", "acme");
    assert!(is_committed(&good), "a self-ref row satisfying a computed-reading row check must be admitted, got {good:?}");
    let bad = add(SELFREF_CHECK_READS_COMPUTED, "srchkcomp-bad", "c2", "");
    assert!(is_check_rejection(&bad), "a self-ref row violating a computed-reading row check must be a Check rejection, got {bad:?}");
}

// ===========================================================================
// PASSING CONTROLS — run by default; fence the boundary so a fix cannot regress the
// neighbouring, currently-working paths.
// ===========================================================================

/// CONTROL: a row `$check` reading the stored FIELD `.name` directly (not the
/// computed) enforces at admission — a non-blank name commits, a blank one is a
/// `Check`. So row checks work; it is reading a COMPUTED value that faults.
#[test]
fn control_row_check_reading_field_enforces() {
    let def = r##"{
      "$liasse": 1, "$app": "t.chkfield@1.0.0",
      "$model": {
        "companies": { "$key": "id", "id": "text", "name": "text",
          "$check": ["size(.name) > 0", "name required"] },
        "$mut": { "add": ".companies + { id: @id, name: @name }" }
      }
    }"##;
    assert!(is_committed(&add(def, "chkfield-good", "c1", "acme")), "a present name satisfies the field-reading row check");
    assert!(is_check_rejection(&add(def, "chkfield-bad", "c2", "")), "a blank name is a Check rejection under the field-reading row check");
}

/// CONTROL: the SAME computed value, with NO check reading it, both admits and
/// materializes through a view — so the computed value and the read path are fine;
/// only the admission-time check read of it faults.
#[test]
fn control_computed_materializes_without_check() {
    let def = r##"{
      "$liasse": 1, "$app": "t.compview@1.0.0",
      "$model": {
        "companies": { "$key": "id", "id": "text", "name": "text", "label": "= .name" },
        "v": { "$view": ".companies { id, label }" },
        "$mut": { "add": ".companies + { id: @id, name: @name }" }
      }
    }"##;
    let mut engine: Engine<MemoryStore> = load("compview", def);
    let mut generator = generator();
    let outcome = engine
        .call(&CallRequest::new("add").arg("id", text("c1")).arg("name", text("acme")), &mut generator)
        .expect("dispatch");
    assert!(is_committed(&outcome), "the collection with a computed but no check-reading-it commits, got {outcome:?}");
    let view = engine.view_at_head("v").expect("view ok").expect("view declared");
    assert_eq!(view.rows()[0].field("label"), Some(&text("acme")), "the computed value materializes through the read path");
}
