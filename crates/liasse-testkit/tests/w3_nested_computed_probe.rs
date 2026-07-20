//! RED-TEAM WAVE 3 (completeness) FINDING — §5.2/§5.4: the runtime is BLIND to a
//! computed value declared inside a NESTED KEYED COLLECTION. It is silently ABSENT
//! everywhere it is read.
//!
//! This is a fresh position in the recurring "runtime is blind to a computed value
//! read indirectly" bug family (cf. the landed cross-collection-computed and
//! struct-nested-ref regressions). §5.2: a computed value "participates in views,
//! checks, sorting, and projections like any other value" — with NO carve-out for a
//! computed declared inside a nested keyed collection (§5.4: "Structs MAY contain
//! fields, structs, sets, views, and nested keyed collections"). So for a company
//! whose nested `departments` each declare `doubled = size * 2`:
//!   * a direct view projecting `d.doubled` must expose 20/10, and
//!   * a parent computed `total = sum(.departments.doubled)` must read them ⇒ 30.
//! Both instead observe `doubled` as absent: the direct view OMITS the member, and
//! the parent `sum` (skipping the absent inputs, §7.5) yields int 0.
//!
//! Root cause (hand-traced): the collection-computed fold descends only into
//! TOP-LEVEL collections and never into a row's nested keyed collections —
//!   * `liasse-runtime/src/eval.rs::fold_collection_computed` (~L257-270) folds
//!     `collection.computed` only for a root cell matched by
//!     `self.compiled.collection(name)` (a top-level collection); a company row's
//!     `departments` cell is left with `doubled` unfolded;
//!   * `liasse-runtime/src/eval.rs::materialize_row_cell` (~L716-724) folds only the
//!     outer `compiled.computed`, not the nested collections' computed values.
//! By contrast `expose_struct_computed`/`fold_struct_computed` DO recurse into
//! nested static structs "to any depth" (eval.rs ~L275), and `expose_root_computed`
//! handles the root — so structs and root are covered; nested keyed collections are
//! the uncovered gap. The implementation calls collection-nested STRUCTS a
//! "documented seam", but §5.2 pins that a computed participates like any other
//! value with no such carve-out, so the outcome is spec-derivable and this is a
//! genuine divergence, not an unpinned gap.
//!
//! Oracle (arithmetic from the seeded inputs alone): departments size 10 and 5
//! ⇒ doubled 20 and 10 ⇒ sum = 30. The plain-field CONTROL sums `.departments.size`
//! ⇒ 15 and PASSES, isolating the defect to the nested COMPUTED specifically —
//! ordinary reads of a nested keyed collection work; only its computed is dropped.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
#![allow(clippy::doc_lazy_continuation)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<w3-nested-computed>"), &BTreeSet::new()).expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("w3-nested-computed"), SuiteKind::Red, &case)
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

// ── FINDING 1 ─────────────────────────────────────────────────────────────────
// §5.2/§5.4: `total = sum(.departments.doubled)` must see the nested computed.
// Observed: total == "0" (nested `doubled` absent, so the sum skips every input).
#[test]
fn parent_computed_aggregating_nested_computed_must_resolve() {
    let text = r##"{
      format: 1
      name: parent-agg-nested-computed
      suite: scenario
      spec: ["#state-model", "§5.2", "§5.4"]
      package: {
        $liasse: 1
        $app: "t.w3.nestcomp@1.0.0"
        $model: {
          companies: {
            $key: "id"
            id: "text"
            departments: { $key: "did", did: "text", size: "int", doubled: "= size * 2" }
            total: "= sum(.departments.doubled)"
          }
          $public: {
            companies: {
              $view: ".companies { id, total }"
              $mut: {
                add_co: ".companies + { id: @id }"
                add_dep: ".companies[@co].departments + { did: @did, size: @size }"
              }
            }
          }
        }
        $data: {}
      }
      steps: [
        { call: "public.companies.add_co", args: { id: "c1" }, expect: { outcome: ok, "...": true } }
        { call: "public.companies.add_dep", args: { co: "c1", did: "d1", size: "10" }, expect: { outcome: ok, "...": true } }
        { call: "public.companies.add_dep", args: { co: "c1", did: "d2", size: "5" }, expect: { outcome: ok, "...": true } }
        // doubled: 20 + 10 = 30
        { watch: "public.companies", id: "w1", expect_init: { value: [ { id: "c1", total: "30" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "parent-agg-nested-computed");
}

// (An admission-pipeline escalation — a `$check` reading the nested computed — was
// probed here but the declared-collection `$check` path cannot see a nested keyed
// collection AT ALL (a distinct defect: `row_cell_of` omits nested collections),
// which confounds it; that separate finding lives in `w3_empty_nested_check_probe.rs`.)

// ── CONTROL A: parent aggregates the nested PLAIN field ────────────────────────
#[test]
fn control_parent_aggregates_nested_plain_field() {
    let text = r##"{
      format: 1
      name: control-parent-agg-nested-plain
      suite: scenario
      spec: ["#state-model", "§5.2", "§5.4"]
      package: {
        $liasse: 1
        $app: "t.w3.nestplain@1.0.0"
        $model: {
          companies: {
            $key: "id"
            id: "text"
            departments: { $key: "did", did: "text", size: "int" }
            total_plain: "= sum(.departments.size)"
          }
          $public: {
            companies: {
              $view: ".companies { id, total_plain }"
              $mut: {
                add_co: ".companies + { id: @id }"
                add_dep: ".companies[@co].departments + { did: @did, size: @size }"
              }
            }
          }
        }
        $data: {}
      }
      steps: [
        { call: "public.companies.add_co", args: { id: "c1" }, expect: { outcome: ok, "...": true } }
        { call: "public.companies.add_dep", args: { co: "c1", did: "d1", size: "10" }, expect: { outcome: ok, "...": true } }
        { call: "public.companies.add_dep", args: { co: "c1", did: "d2", size: "5" }, expect: { outcome: ok, "...": true } }
        { watch: "public.companies", id: "w1", expect_init: { value: [ { id: "c1", total_plain: "15" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-parent-agg-nested-plain");
}

// ── FINDING 2 ─────────────────────────────────────────────────────────────────
// §5.2/§5.4: a direct view projecting the nested computed `d.doubled` must expose
// 20/10. Observed: the `doubled` member is ABSENT on every projected department row
// (same root gap — the nested-collection computed is never folded).
#[test]
fn direct_view_of_nested_computed_must_expose_it() {
    let text = r##"{
      format: 1
      name: control-direct-view-nested-computed
      suite: scenario
      spec: ["#state-model", "§5.2", "§5.4"]
      package: {
        $liasse: 1
        $app: "t.w3.nestdirect@1.0.0"
        $model: {
          companies: {
            $key: "id"
            id: "text"
            departments: { $key: "did", did: "text", size: "int", doubled: "= size * 2" }
          }
          $public: {
            companies: {
              $mut: {
                add_co: ".companies + { id: @id }"
                add_dep: ".companies[@co].departments + { did: @did, size: @size }"
              }
            }
            deps: { $view: ".companies[:c].departments[:d] { comp: c.id, dep: d.did, doubled: d.doubled }" }
          }
        }
        $data: {}
      }
      steps: [
        { call: "public.companies.add_co", args: { id: "c1" }, expect: { outcome: ok, "...": true } }
        { call: "public.companies.add_dep", args: { co: "c1", did: "d1", size: "10" }, expect: { outcome: ok, "...": true } }
        { call: "public.companies.add_dep", args: { co: "c1", did: "d2", size: "5" }, expect: { outcome: ok, "...": true } }
        { watch: "public.deps", id: "w1", expect_init: { value: { $unordered: [
          { comp: "c1", dep: "d1", doubled: "20" }, { comp: "c1", dep: "d2", doubled: "10" }
        ] } } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-direct-view-nested-computed");
}
