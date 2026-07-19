//! RED TEAM — §5.3 static structs × §8 admission-time rule evaluation. The
//! fdc7639 env-shaping landing wired `^` lexical-parent scope (§6.2) and static
//! structs into the READ/VIEW env (materialization, projections). This probes the
//! ADMISSION path (§8.8 checks/asserts), where two static-struct evaluations still
//! fault instead of enforcing.
//!
//! # Findings (open bugs; the `#[ignore]`d repros pin them)
//!
//! **F-A (`^` in a static-struct `$check`).** A `$check` declared inside a static
//! struct (§5.3) that reads the containing row through `^` (§6.2) type-checks at
//! load but, at admission, FAULTS on every insert — good and bad alike — with
//! `Rejection{Evaluation}: "environment supplied a value that is not a current
//! value at this scope depth"`. So a collection carrying such a struct is entirely
//! uninsertable. Root cause: the admission check is evaluated with a single scope
//! frame (the struct row), so `^` (scope depth 1) exceeds the frame stack in
//! `Evaluator::current_at` (crates/liasse-expr/src/eval/mod.rs:124-133); the read
//! path supplies the parent chain via `evaluate_scoped` (eval.rs `fold_struct_computed`),
//! but the §8 check path does not.
//!
//! **F-B (row `$check` reading a static-struct member).** A row-level `$check`
//! (§5.10) that reads a nested static-struct member (`.meta.tag == .name`) FAULTS at
//! admission with `Rejection{Evaluation}: "... not a row with this field"`, because
//! the prospective row carries `meta` as a `Value::Struct` scalar and member access
//! `.tag` expects a `Cell::Row`. The IDENTICAL read via a re-materialized row in a
//! mutation `return`/`assert` (`.people[@id].meta.tag`) works — so the fault is
//! specific to the row-check's prospective `.`.
//!
//! ## Why these are bugs
//!
//! §5.2/§5.10 state that computed values and checks "participate ... like any other
//! value", and §6.2 defines `^` as the lexical parent a struct-declared check reads
//! the containing row through. Neither is restricted from admission-time checks.
//! Both faults are fail-closed (a false `Evaluation` refusal, never a silent
//! commit), so they are correctness/usability gaps, not integrity or authz breaches.
//! The `must_enforce` repros assert the SPEC-correct admission outcome (accept the
//! satisfying row, reject the violating one as a `Check`) and are `#[ignore]`d; run
//! with `--ignored` to reproduce. The PASSING controls run by default and fence the
//! boundary. Every expectation is deducible from SPEC.md alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, RejectionReason, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// Run `add(id, name, tag)` on a fresh copy of `definition` and return the outcome.
fn add(definition: &str, name_instance: &str, tag: &str) -> CallOutcome {
    let mut engine: Engine<MemoryStore> = load(name_instance, definition);
    let mut generator = generator();
    engine
        .call(
            &CallRequest::new("add")
                .arg("id", text("p1"))
                .arg("name", text("acme"))
                .arg("tag", text(tag)),
            &mut generator,
        )
        .expect("call dispatches (an admission refusal is an outcome, not an error)")
}

fn is_committed(o: &CallOutcome) -> bool {
    matches!(o, CallOutcome::Committed { .. })
}

fn is_check_rejection(o: &CallOutcome) -> bool {
    matches!(o, CallOutcome::Rejected(r) if r.reason() == RejectionReason::Check)
}

// ===========================================================================
// F-A — `^` in a static-struct `$check`. OPEN BUG repro.
// ===========================================================================

/// The `meta.$check` requires `tag == ^.name` (§6.2 parent). A row whose struct
/// `tag` equals the parent `name` MUST be admitted; one that differs MUST be
/// rejected as a `Check`. Today both FAULT with an `Evaluation` scope-depth error.
const CARET_CHECK: &str = r##"{
  "$liasse": 1,
  "$app": "t.caretchk@1.0.0",
  "$model": {
    "people": {
      "$key": "id", "id": "text", "name": "text",
      "meta": { "tag": "text", "$check": [".tag == ^.name", "tag must equal the row name"] }
    },
    "$mut": { "add": ".people + { id: @id, name: @name, meta: { tag: @tag } }" }
  }
}"##;

#[test]
fn caret_in_struct_check_must_enforce() {
    // SPEC-correct: satisfying row admitted.
    let good = add(CARET_CHECK, "caret-good", "acme");
    assert!(is_committed(&good), "a struct row satisfying its `^` check must be admitted, got {good:?}");
    // SPEC-correct: violating row rejected as a Check (not an Evaluation fault).
    let bad = add(CARET_CHECK, "caret-bad", "WRONG");
    assert!(is_check_rejection(&bad), "a struct row violating its `^` check must be a Check rejection, got {bad:?}");
}

// ===========================================================================
// F-B — row `$check` reading a static-struct member. OPEN BUG repro.
// ===========================================================================

/// A row-level `$check` reads a static-struct member `.meta.tag` and compares it to
/// `.name`. A satisfying row MUST admit; a violating one MUST be a `Check`. Today
/// both FAULT with `Evaluation: not a row with this field`.
const ROW_CHECK_STRUCT_MEMBER: &str = r##"{
  "$liasse": 1,
  "$app": "t.rowchk@1.0.0",
  "$model": {
    "people": {
      "$key": "id", "id": "text", "name": "text",
      "meta": { "tag": "text" },
      "$check": [".meta.tag == .name", "tag must equal the row name"]
    },
    "$mut": { "add": ".people + { id: @id, name: @name, meta: { tag: @tag } }" }
  }
}"##;

#[test]
fn row_check_reading_struct_member_must_enforce() {
    let good = add(ROW_CHECK_STRUCT_MEMBER, "rowchk-good", "acme");
    assert!(is_committed(&good), "a row satisfying a struct-member row check must be admitted, got {good:?}");
    let bad = add(ROW_CHECK_STRUCT_MEMBER, "rowchk-bad", "WRONG");
    assert!(is_check_rejection(&bad), "a row violating a struct-member row check must be a Check rejection, got {bad:?}");
}

// ===========================================================================
// PASSING CONTROLS — run by default; fence the boundary so a fix cannot regress
// the neighbouring, currently-working admission paths.
// ===========================================================================

/// CONTROL: a static-struct `$check` with NO `^` — self-only, `.tag != ''` — is
/// enforced at admission (a blank tag is rejected as a `Check`, a present tag
/// commits). So struct checks work; it is the `^` frame specifically that faults.
#[test]
fn control_struct_check_without_caret_enforces() {
    let def = r##"{
      "$liasse": 1, "$app": "t.nc@1.0.0",
      "$model": {
        "people": { "$key": "id", "id": "text", "name": "text",
          "meta": { "tag": "text", "$check": [".tag != ''", "tag required"] } },
        "$mut": { "add": ".people + { id: @id, name: @name, meta: { tag: @tag } }" }
      }
    }"##;
    assert!(is_committed(&add(def, "nc-good", "present")), "a present tag satisfies the self-only struct check");
    assert!(is_check_rejection(&add(def, "nc-bad", "")), "a blank tag is a Check rejection under the self-only struct check");
}

/// CONTROL: reading a static-struct member `.meta.tag` in a mutation `return`
/// (re-materialized committed row) works — the member is present in the projected
/// response, so struct-member READ itself is fine off a materialized row.
#[test]
fn control_return_reads_struct_member() {
    let def = r##"{
      "$liasse": 1, "$app": "t.ret@1.0.0",
      "$model": {
        "people": { "$key": "id", "id": "text", "meta": { "tag": "text" } },
        "$mut": { "add": [".people + { id: @id, meta: { tag: @tag } }", "return .people[@id] { id, t: .meta.tag }"] }
      }
    }"##;
    let mut engine: Engine<MemoryStore> = load("ret", def);
    let mut generator = generator();
    let outcome = engine
        .call(&CallRequest::new("add").arg("id", text("p1")).arg("tag", text("hello")), &mut generator)
        .expect("dispatch");
    let response = outcome.response().expect("a return projects the struct member");
    // The `t` output field carries the struct member value, proving member READ works.
    assert!(
        format!("{response:?}").contains("hello"),
        "the mutation return must carry the struct member `.meta.tag`, got {response:?}"
    );
}

/// CONTROL: a mutation `assert` reading a static-struct member off a re-materialized
/// row (`.people[@id].meta.tag == @name`) enforces — the satisfying insert commits.
/// This is the working counterpart to F-B's faulting row-`$check` read.
#[test]
fn control_assert_reads_struct_member() {
    let def = r##"{
      "$liasse": 1, "$app": "t.asrt@1.0.0",
      "$model": {
        "people": { "$key": "id", "id": "text", "name": "text", "meta": { "tag": "text" } },
        "$mut": { "add": [".people + { id: @id, name: @name, meta: { tag: @tag } }",
                          "assert(.people[@id].meta.tag == @name, 'tag must equal name')"] }
      }
    }"##;
    assert!(is_committed(&add(def, "asrt-good", "acme")), "a satisfying assert over a re-materialized struct member commits");
}
