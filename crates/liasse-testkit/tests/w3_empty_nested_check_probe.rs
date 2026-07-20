//! RED-TEAM WAVE 3 (completeness) FINDING — §8.8/§5.5/§5.4: an admission-time
//! `$check` CANNOT SEE a row's nested keyed collections. Any `$check` that reads
//! (aggregates over) a nested keyed child collection FAULTS and rejects otherwise
//! valid state.
//!
//! §5.5 (pinned by the passing corpus case
//! `tests/05-state-model/common/omitted-child-collections-start-empty.hjson`): a
//! nested keyed collection is real row state — omitted, it "starts empty, not
//! absent"; populated, it holds its rows. §7.5: an empty aggregate yields its
//! identity (`count -> 0`, `sum -> 0`). §8.8: a `$check` must hold in every
//! committed state, and a state that satisfies it is admitted. So `count(.departments)
//! >= 0` / `sum(.departments.size) >= 0` are trivially true (`n >= 0`) and MUST admit.
//!
//! Observed: every such check REJECTS the transition (a mutation insert is rejected;
//! a genesis seed carrying the same check fails to load with "environment supplied a
//! value that is not a row with this field"). The defect is NOT specific to an empty
//! child — a seed of a POPULATED nested collection under the same check also faults —
//! so it is not the §7.5 empty case but the nested collection being INVISIBLE to the
//! check evaluator entirely.
//!
//! Root cause (hand-traced): an admission `$check` evaluates over `ctx.row_cell_of`
//! (`liasse-runtime/src/rules.rs:573` collection check, `:607` field check), and
//! `row_cell_of` (`liasse-runtime/src/eval.rs:736`) wraps the bare `row_cell`
//! (`eval.rs:893`), which builds the row from `collection.fields` (scalars/refs/sets)
//! and `collection.structs` (static structs, as `Value::Struct`) ONLY — it never
//! includes the row's NESTED KEYED COLLECTIONS (contrast `materialize_row_cell`,
//! `eval.rs:704`, which "includes nested collections and static structs"). So the
//! check's `.departments` navigation hits an absent member and faults with
//! `EvalError::ShapeMismatch { expected: "a row with this field" }`
//! (`liasse-expr/src/eval/mod.rs:282`); the fault fails closed, rejecting the
//! transition. Static structs ARE present in `row_cell`, so a struct-field check
//! works — the gap is nested keyed collections specifically.
//!
//! Isolation:
//!   * `control_check_without_nested_ref_admits` — the identical insert under a
//!     `1 >= 0` check (no nested reference) ADMITS: the check machinery is sound.
//!   * `control_seeded_nested_reads_back_without_check` — a company seeded WITH a
//!     department, no check, loads and the nested rows read back through a view
//!     (`[{comp,dep,size}]`), proving the nested state is real and readable (§5.5);
//!     only the check evaluator cannot see it.
//!
//! Every expectation is derived from SPEC.md (§5.5, §7.5, §8.8) + the input alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
#![allow(clippy::doc_lazy_continuation)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<w3-nested-check>"), &BTreeSet::new()).expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("w3-nested-check"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult, name: &str) {
    let ok = result.steps.iter().all(|s| s.result.is_pass());
    if !ok {
        for step in &result.steps {
            println!(
                "  {name} step {} [{}] -> {:?} observed={:?}",
                step.index, step.action, step.result, step.observed
            );
        }
    }
    assert!(ok, "{name}: a step diverged from its SPEC-derived expectation (see dump)");
}

// ── FINDING 1: count() over a nested collection in a check (empty child) ───────
#[test]
fn check_count_over_nested_collection_must_admit() {
    let text = r##"{
      format: 1
      name: check-count-nested-collection
      suite: scenario
      spec: ["#state-model", "§5.5", "#views", "§7.5", "#mutations", "§8.8"]
      package: {
        $liasse: 1
        $app: "t.w3.nestcount@1.0.0"
        $model: {
          companies: {
            $key: "id"
            id: "text"
            departments: { $key: "did", did: "text", size: "int" }
            $check: ["count(.departments) >= 0"]
          }
          $public: { companies: { $view: ".companies { id }", $mut: { add_co: ".companies + { id: @id }" } } }
        }
        $data: {}
      }
      steps: [
        // §5.5: departments starts empty; §7.5: count(empty) = 0; 0 >= 0 holds ⇒ admit.
        { call: "public.companies.add_co", args: { id: "c1" }, expect: { outcome: ok, "...": true } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "check-count-nested-collection");
}

// ── FINDING 2: sum() over a nested collection in a check (empty child) ─────────
#[test]
fn check_sum_over_nested_collection_must_admit() {
    let text = r##"{
      format: 1
      name: check-sum-nested-collection
      suite: scenario
      spec: ["#state-model", "§5.5", "#views", "§7.5", "#mutations", "§8.8"]
      package: {
        $liasse: 1
        $app: "t.w3.nestsum@1.0.0"
        $model: {
          companies: {
            $key: "id"
            id: "text"
            departments: { $key: "did", did: "text", size: "int" }
            $check: ["sum(.departments.size) >= 0"]
          }
          $public: { companies: { $view: ".companies { id }", $mut: { add_co: ".companies + { id: @id }" } } }
        }
        $data: {}
      }
      steps: [
        { call: "public.companies.add_co", args: { id: "c1" }, expect: { outcome: ok, "...": true } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "check-sum-nested-collection");
}

// ── FINDING 3: the SAME check over a POPULATED (seeded) nested collection also
//    faults — proving the defect is the nested collection being invisible to the
//    check, not the §7.5 empty case. The genesis seed must admit (count = 1 >= 0)
//    and the company must read back; instead the package fails to load.
#[test]
fn check_over_populated_nested_collection_must_load() {
    let text = r##"{
      format: 1
      name: check-populated-nested-collection
      suite: scenario
      spec: ["#state-model", "§5.5", "#mutations", "§8.8"]
      package: {
        $liasse: 1
        $app: "t.w3.nestpop@1.0.0"
        $model: {
          companies: {
            $key: "id"
            id: "text"
            departments: { $key: "did", did: "text", size: "int" }
            $check: ["count(.departments) >= 0"]
          }
          $public: { companies: { $view: ".companies { id }" } }
        }
        $data: { companies: { c1: { departments: { d1: { size: "10" } } } } }
      }
      steps: [
        { watch: "public.companies", id: "w1", expect_init: { value: [ { id: "c1" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "check-populated-nested-collection");
}

// ── CONTROL A: a check with NO nested reference admits the same insert ─────────
#[test]
fn control_check_without_nested_ref_admits() {
    let text = r##"{
      format: 1
      name: control-check-no-nested-ref
      suite: scenario
      spec: ["#mutations", "§8.8"]
      package: {
        $liasse: 1
        $app: "t.w3.nestctl0@1.0.0"
        $model: {
          companies: {
            $key: "id"
            id: "text"
            departments: { $key: "did", did: "text", size: "int" }
            $check: ["1 >= 0"]
          }
          $public: { companies: { $view: ".companies { id }", $mut: { add_co: ".companies + { id: @id }" } } }
        }
        $data: {}
      }
      steps: [
        { call: "public.companies.add_co", args: { id: "c1" }, expect: { outcome: ok, "...": true } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-check-no-nested-ref");
}

// ── CONTROL B: the nested state is real and readable (no check) ────────────────
#[test]
fn control_seeded_nested_reads_back_without_check() {
    let text = r##"{
      format: 1
      name: control-seeded-nested-reads
      suite: scenario
      spec: ["#state-model", "§5.5"]
      package: {
        $liasse: 1
        $app: "t.w3.nestctl1@1.0.0"
        $model: {
          companies: { $key: "id", id: "text", departments: { $key: "did", did: "text", size: "int" } }
          $public: {
            companies: { $view: ".companies { id }" }
            deps: { $view: ".companies[:c].departments[:d] { comp: c.id, dep: d.did, size: d.size }" }
          }
        }
        $data: { companies: { c1: { departments: { d1: { size: "10" } } } } }
      }
      steps: [
        { watch: "public.companies", id: "w1", expect_init: { value: [ { id: "c1" } ] } }
        { watch: "public.deps", id: "w2", expect_init: { value: [ { comp: "c1", dep: "d1", size: "10" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-seeded-nested-reads");
}
