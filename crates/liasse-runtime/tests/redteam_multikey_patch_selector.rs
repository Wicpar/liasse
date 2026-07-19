#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM (§6.3 / §8.6): an in-program patch or field write over a
//! *multi-operand* keyed selector must act on EVERY selected row.
//!
//! §6.3 (line 715): "Comma-separated selector operands are independent key
//! sources, and their selected rows are concatenated in operand order."
//! §6.3 (line 717): "Every selector returns a row view. One scalar or composite
//! key contributes zero rows when the key is absent and one row when it exists."
//! §8.6: "A patch on a row source applies to every selected row."
//!
//! The corpus already fixes this reading: `06-expressions/red/
//! patch-duplicate-selector-applies-once.hjson` treats `.counters[@a, @b] { … }`
//! as a BULK patch over its selected rows (deduplicated by incarnation, §6.3).
//! That case only aliases ONE row (`a=c1, b=c1`), so it passes even against an
//! implementation that looks at the first operand alone. These probes use
//! DISTINCT keys, so they fail iff the implementation drops operands past the
//! first.
//!
//! ROOT CAUSE: `crate::interp::Interp::row_target`
//! (crates/liasse-runtime/src/interp.rs:1717-1726) resolves a keyed selector
//! from `keys.first()` only, and `patch_plan`
//! (crates/liasse-runtime/src/interp.rs:1590-1619) routes every non-`Bind`
//! selector through `row_target` as a single-row `PatchPlan::Single`. A
//! multi-operand selector therefore acts on the first operand's row and silently
//! ignores the rest. The delete path (`exec_delete`, which iterates the operand
//! set) is unaffected — see the passing control below — which isolates the fault
//! to the patch / field-write receiver resolution.
//!
//! Severity: HIGH. A well-formed mutation reports `committed` success while
//! applying only part of its declared write (a committed-state integrity gap,
//! §22.1), or rejects a valid patch outright when the ignored operand happens to
//! be listed first.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

type Eng = Engine<MemoryStore>;

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// A minimal keyed collection with a boolean flag, a bulk patch and a bulk field
/// write over a two-operand selector, plus a two-key delete used as the control.
const PKG: &str = r#"{
  "$liasse": 1
  "$app": "t.multikey@1.0.0"
  "$model": {
    "tasks": { "$key": "id", "id": "text", "done": "bool = false" }
    "done_view": { "$view": ".tasks[:t | t.done] { id, $sort: [id] }" }
    "live_view": { "$view": ".tasks { id, $sort: [id] }" }
    "$mut": {
      "patch2({ a: text, b: text })": ".tasks[@a, @b] { done = true }"
      "write2({ a: text, b: text })": ".tasks[@a, @b].done = true"
      "mark({ id: text })": ".tasks[@id] { done = true }"
      "drop2({ a: text, b: text })": ".tasks - [@a, @b]"
    }
  }
  "$data": { "tasks": { "t1": {}, "t2": {}, "t3": {} } }
}"#;

fn ids_of(engine: &Eng, view: &str) -> Vec<String> {
    let result = engine.view_at_head(view).expect("view").expect("declared");
    result
        .rows()
        .iter()
        .map(|r| match r.field("id") {
            Some(Value::Text(t)) => t.as_str().to_owned(),
            other => panic!("id: {other:?}"),
        })
        .collect()
}

fn call(engine: &mut Eng, mutation: &str, args: &[(&str, &str)]) -> CallOutcome {
    let mut g = generator();
    let mut request = CallRequest::new(mutation);
    for (name, value) in args {
        request = request.arg(*name, text(value));
    }
    engine.call(&request, &mut g).expect("the call reaches admission")
}

/// THE FINDING. `.tasks[@a, @b] { done = true }` with two DISTINCT keys must
/// patch BOTH selected rows (§6.3 concatenates the operands' rows; §8.6 patches
/// every selected row). The implementation patches only the first operand's row
/// (`t1`) and reports `committed`, silently dropping the write on `t2`.
#[test]
fn multikey_patch_must_apply_to_every_selected_row() {
    let mut engine = load("multikey", PKG);
    let outcome = call(&mut engine, "patch2", &[("a", "t1"), ("b", "t2")]);
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the patch commits: {outcome:?}");
    assert_eq!(
        ids_of(&engine, "done_view"),
        vec!["t1".to_owned(), "t2".to_owned()],
        "§6.3/§8.6: BOTH selected rows must be patched; the engine patched only the first operand's row",
    );
}

/// FINDING facet 2 — same root cause via a field write. `.tasks[@a, @b].done =
/// true` must set the flag on both selected rows.
#[test]
fn multikey_field_write_must_apply_to_every_selected_row() {
    let mut engine = load("multikey", PKG);
    call(&mut engine, "write2", &[("a", "t1"), ("b", "t2")]);
    assert_eq!(
        ids_of(&engine, "done_view"),
        vec!["t1".to_owned(), "t2".to_owned()],
        "§6.3/§8.6: a multi-key field write must set the field on every selected row",
    );
}

/// FINDING facet 3 — the failure inverts when the ignored operand is listed
/// first. `.tasks[@absent, @present] { done = true }`: §6.3 makes the absent key
/// contribute zero rows, so the present row is patched and the call commits. The
/// engine instead takes the first operand alone, finds it absent, and rejects the
/// whole call with `MissingTarget` — refusing a valid bulk patch.
#[test]
fn multikey_patch_with_absent_leading_key_still_patches_the_present_row() {
    let mut engine = load("multikey", PKG);
    let outcome = call(&mut engine, "patch2", &[("a", "zz"), ("b", "t2")]);
    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "an absent leading operand contributes zero rows; the present t2 must still be patched (got {outcome:?})",
    );
    assert_eq!(ids_of(&engine, "done_view"), vec!["t2".to_owned()], "t2 is patched");
}

// ---------------------------------------------------------------------------
// PASSING CONTROLS — these isolate the fault to the patch / field-write path.
// ---------------------------------------------------------------------------

/// CONTROL: the DELETE path over the same two-operand key set removes BOTH named
/// rows. `exec_delete` iterates the operand set, so the multi-operand handling is
/// correct here — proving the collection layer supports it and the defect is
/// specific to `row_target`/`patch_plan`.
#[test]
fn control_multikey_delete_removes_every_named_row() {
    let mut engine = load("multikey", PKG);
    call(&mut engine, "drop2", &[("a", "t1"), ("b", "t2")]);
    assert_eq!(ids_of(&engine, "live_view"), vec!["t3".to_owned()], "both t1 and t2 are deleted; t3 remains");
}

/// CONTROL: a single-key patch is applied, and a single-key patch on an absent
/// row rejects (§8.9). Both hold — the single-operand path is correct.
#[test]
fn control_single_key_patch_and_missing_target() {
    let mut engine = load("multikey", PKG);
    let ok = call(&mut engine, "mark", &[("id", "t1")]);
    assert!(matches!(ok, CallOutcome::Committed { .. }), "a single-key patch commits: {ok:?}");
    assert_eq!(ids_of(&engine, "done_view"), vec!["t1".to_owned()]);

    let missing = call(&mut engine, "mark", &[("id", "nope")]);
    assert!(
        matches!(missing, CallOutcome::Rejected(_)),
        "§8.9: a single keyed patch on an absent row rejects, got {missing:?}",
    );
}
