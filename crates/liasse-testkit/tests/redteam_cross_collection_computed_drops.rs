//! RED-TEAM finding (WAVE 2) — §5.2: a computed value that reads ANOTHER
//! collection's computed value through a `/`-selector is silently dropped.
//!
//! §5.2: "A computed value ... participates in views, checks, sorting, and
//! projections **like any other value**." The wave-1 computed-type inference pass
//! (`liasse-model/src/infer.rs`) was built explicitly to support this: its own doc
//! states "a computed value may read a sibling (or, through `/`, a cross-shape)
//! computed value" and "the package root `/` is rebuilt each pass so a computed
//! value reading a refined field through a `/collection[...]` selector observes the
//! refinement (§5.3)." So `items.derived = /config["main"].doubled + 1`, where
//! `config.doubled` is itself a computed value, type-checks and loads.
//!
//! Observed divergence: at runtime `derived` is ABSENT — the read of
//! `/config["main"].doubled` sees `config` WITHOUT its computed values folded, so
//! `.doubled` resolves to nothing and the whole `derived` expression yields none.
//!
//! Root cause (hand-traced): `liasse-runtime/src/eval.rs::expose_computed` (~L225)
//! folds each collection's computed values using an environment built from the
//! `base` root that has NOT yet had computed values folded, and there is no
//! cross-collection fixed point (the only fixed points are per-row —
//! `fold_computed_scoped` — and root-to-root — `expose_root_computed`). A
//! collection computed value that reads another collection's computed value through
//! `/` therefore observes the unfolded target and reads the sibling computed as
//! absent. `Row::root` (~L192) runs `expose_computed` before `expose_root_computed`
//! with no second pass, so the omission cannot self-correct.
//!
//! Controls isolate the defect exactly:
//!   * `control_cross_collection_plain_field` — reading a PLAIN sibling field
//!     (`/config["main"].base`) through the same computed works, so `/`-selector
//!     cross-collection reads themselves are sound; only the computed target fails.
//!   * `control_same_row_computed_chain` — a computed reading a same-row computed
//!     works, so the per-row computed fold is sound.
//!   * `control_direct_view_of_sibling_computed` — reading `config.doubled`
//!     directly through a view materializes it as `20`, so the computed value
//!     itself is correct; it is only invisible when read from another collection's
//!     computed.
//!
//! Every expectation is arithmetic derivable from SPEC.md and `$data` alone
//! (base=10 -> doubled=20 -> derived=21), never from observed behaviour.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<cross-collection-computed>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("cross-collection-computed"), SuiteKind::Red, &case)
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

// ── THE FINDING ──────────────────────────────────────────────────────────────
// §5.2: `derived` reads `config`'s computed `doubled`. Expect derived = 21.
#[test]
fn cross_collection_computed_read_must_resolve() {
    let text = r##"{
      format: 1
      name: cross-collection-computed-read
      suite: scenario
      spec: ["#state-model", "§5.2", "§5.3"]
      package: {
        $liasse: 1
        $app: "t.xcc.finding@1.0.0"
        $model: {
          config: { $key: "k", k: "text", base: "int", doubled: "= base * 2" }
          items: { $key: "id", id: "text", derived: "= /config[\"main\"].doubled + 1" }
          $mut: { add: ".items + { id: @id }" }
          $public: { items: { $view: ".items { id, derived }", $mut: { add: ".add" } } }
        }
        $data: { config: { main: { base: "10" } } }
      }
      steps: [
        { call: "public.items.add", args: { id: "i1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.items", id: "w1", expect_init: { value: [ { id: "i1", derived: "21" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "cross-collection-computed-read");
}

// ── CONTROL: cross-collection read of a PLAIN field works ─────────────────────
#[test]
fn control_cross_collection_plain_field() {
    let text = r##"{
      format: 1
      name: control-cross-collection-plain
      suite: scenario
      spec: ["#state-model", "§5.2"]
      package: {
        $liasse: 1
        $app: "t.xcc.plain@1.0.0"
        $model: {
          config: { $key: "k", k: "text", base: "int" }
          items: { $key: "id", id: "text", derived: "= /config[\"main\"].base + 1" }
          $mut: { add: ".items + { id: @id }" }
          $public: { items: { $view: ".items { id, derived }", $mut: { add: ".add" } } }
        }
        $data: { config: { main: { base: "10" } } }
      }
      steps: [
        { call: "public.items.add", args: { id: "i1" }, expect: { outcome: ok, "...": true } }
        { watch: "public.items", id: "w1", expect_init: { value: [ { id: "i1", derived: "11" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-cross-collection-plain");
}

// ── CONTROL: same-row computed chain works ────────────────────────────────────
#[test]
fn control_same_row_computed_chain() {
    let text = r##"{
      format: 1
      name: control-same-row-computed-chain
      suite: scenario
      spec: ["#state-model", "§5.2"]
      package: {
        $liasse: 1
        $app: "t.xcc.samerow@1.0.0"
        $model: {
          items: { $key: "id", id: "text", base: "int", doubled: "= base * 2", plus1: "= .doubled + 1" }
          $mut: { add: ".items + { id: @id, base: @b }" }
          $public: { items: { $view: ".items { id, plus1 }", $mut: { add: ".add" } } }
        }
        $data: {}
      }
      steps: [
        { call: "public.items.add", args: { id: "i1", b: "10" }, expect: { outcome: ok, "...": true } }
        { watch: "public.items", id: "w1", expect_init: { value: [ { id: "i1", plus1: "21" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-same-row-computed-chain");
}

// ── CONTROL: the sibling computed is correct when read directly through a view ─
#[test]
fn control_direct_view_of_sibling_computed() {
    let text = r##"{
      format: 1
      name: control-direct-view-sibling-computed
      suite: scenario
      spec: ["#state-model", "§5.2"]
      package: {
        $liasse: 1
        $app: "t.xcc.direct@1.0.0"
        $model: {
          config: { $key: "k", k: "text", base: "int", doubled: "= base * 2" }
          $public: { config: { $view: ".config { k, doubled }" } }
        }
        $data: { config: { main: { base: "10" } } }
      }
      steps: [
        { watch: "public.config", id: "w1", expect_init: { value: [ { k: "main", doubled: "20" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-direct-view-sibling-computed");
}
