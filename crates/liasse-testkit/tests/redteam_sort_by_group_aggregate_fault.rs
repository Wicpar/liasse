//! RED-TEAM finding (§7.3 `$sort` over a grouped `$view` referencing the `group`
//! binding): a grouped projection whose `$sort` key reads the synthetic-group
//! `group` binding DIRECTLY (`-count(group)`, `sum(group.f)`) — rather than a
//! named projected output that wraps that aggregate — type-checks cleanly but
//! FAULTS at read time with an unbound `group` binding, surfacing as an
//! engine-invariant fault instead of the sorted row stream the SPEC mandates.
//!
//! # What the SPEC pins
//!
//! §7.2: "A projection MAY declare a synthetic `$key` for grouping … Rows sharing
//! the synthetic key form one group." §7.5: `count(group)` / `sum(group.f)` are
//! the aggregates over that group's source-row view — `group` is the in-scope
//! binding naming the group. §7.3: "The `$sort` array lists successive comparison
//! keys … Sort expressions compare lexicographically." A sort key is any
//! comparison-key expression visible in the projection frame, so in a grouped view
//! `$sort: ["-count(group)"]` (order groups by descending size) is well-formed:
//! `group` is in scope for the sort key exactly as it is for an output, and
//! `count(group)` is a scalar `int` comparison key.
//!
//! With items grouped by `cat` — cat "a" (1 row) and cat "b" (3 rows) — descending
//! by `count(group)` MUST yield group "b" (count 3) before group "a" (count 1):
//! `[{k:"b"}, {k:"a"}]`. Every value is hand-derived from the seed and is the
//! REVERSE of the key-ascending default order (`[{k:"a"}, {k:"b"}]`), so the sort
//! is load-bearing and the case is non-tautological.
//!
//! # The divergence (root cause, hand-traced)
//!
//! The checker binds `group` into the projection frame before checking `$sort`:
//!   * `crates/liasse-expr/src/check/project.rs:133-135` binds
//!     `group -> View(source_row)`; `:207` runs `check_sort` while that binding is
//!     still live (the frame is popped at `:208`), so `-count(group)` type-checks.
//!
//! The evaluator's sort-key evaluation does NOT replicate that binding:
//!   * `crates/liasse-expr/src/eval/views.rs::eval_keys` (`:297-333`) pushes `.`
//!     (`:303`), binds the source-chain `scope.binds` (`:308-311`) and the
//!     projected outputs (`:312-315`), but NEVER binds `group` — unlike
//!     `project_row` (`:249-252`) and (post-fix) `group_key` (`:342-344`), which
//!     both bind it. So the sort key `count(group)` resolves an unbound `group`
//!     and raises `EvalError::UnboundName { name: "group" }`.
//!   * That read-time rejection is wrapped into `EngineError::Internal` at
//!     `crates/liasse-runtime/src/engine.rs:1470` ("engine invariant violated"),
//!     failing the whole watch instead of returning the sorted stream.
//!
//! This is the SAME missing-`group`-binding defect that was fixed in `group_key`
//! (see `redteam_grouped_key_from_binding_fault`), left unfixed in `eval_keys`.
//!
//! Controls isolate the fault precisely:
//!   * `control_sort_via_named_group_output` — the identical order, but the group
//!     count is a NAMED output (`n: count(group)`) and `$sort: ["-n"]` references
//!     that output. `eval_keys` DOES bind projected outputs, so this PASSES —
//!     proving grouped sorting itself works and the gap is the direct `group`
//!     reference in the sort key.
//!   * `control_grouped_default_order` — the same grouped view with no `$sort`
//!     yields the key-ascending default `[{k:"a"}, {k:"b"}]`, proving grouping and
//!     the seed are sound.
//!
//! Every expectation follows from SPEC.md text alone (§7.2, §7.3, §7.5, §B.5),
//! never from observed engine behaviour.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

fn run(app: &str) -> CaseResult {
    let case = Case::from_hjson(app, Path::new("<redteam-sort-by-group-aggregate>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("07-views"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult) {
    for (index, step) in result.steps.iter().enumerate() {
        assert!(
            step.result.is_pass(),
            "step {index} did not pass: observed={:?} result={:?}",
            step.observed,
            step.result
        );
    }
}

// ── THE FINDING ────────────────────────────────────────────────────────────
// §7.3/§7.5: `$sort: ["-count(group)"]` orders the two groups by descending size.
// cat "b" has 3 rows, cat "a" has 1, so the order is [b, a] — the reverse of the
// key-ascending default. FAILS today: the read faults with unbound `group`.
const SORT_BY_GROUP_COUNT: &str = r##"{
  format: 1
  name: sort-by-group-count
  suite: scenario
  spec: ["#views","§7.3","§7.5","§7.2","§B.5"]
  package: { $liasse:1, $app:"t.sortgroup@1.0.0", $model: {
    items: { $key:"id", id:"text", cat:"text" }
    $public: {
      by_size: { $view: ".items { $key: k, k: cat, $sort: [\"-count(group)\"] }" }
    }
  }, $data: { items: {
    a1:{cat:"a"}, b1:{cat:"b"}, b2:{cat:"b"}, b3:{cat:"b"}
  } } }
  steps: [ { watch:"public.by_size", id:"w1", expect_init: { value: [
    { k:"b" },
    { k:"a" }
  ] } } ]
}"##;

#[test]
fn sort_by_group_count_direct() {
    assert_all_pass(&run(SORT_BY_GROUP_COUNT));
}

// ── CONTROL: the identical order via a NAMED group-count output PASSES ────────
// `n: count(group)` is a projected output; `eval_keys` binds projected outputs,
// so `$sort: ["-n"]` resolves cleanly. Isolates the fault to the direct `group`
// reference inside the sort key.
const SORT_VIA_NAMED_OUTPUT: &str = r##"{
  format: 1
  name: sort-via-named-group-output
  suite: scenario
  spec: ["#views","§7.3","§7.5","§7.2","§B.5"]
  package: { $liasse:1, $app:"t.sortgroupctl@1.0.0", $model: {
    items: { $key:"id", id:"text", cat:"text" }
    $public: {
      by_size: { $view: ".items { $key: k, k: cat, n: count(group), $sort: [\"-n\"] }" }
    }
  }, $data: { items: {
    a1:{cat:"a"}, b1:{cat:"b"}, b2:{cat:"b"}, b3:{cat:"b"}
  } } }
  steps: [ { watch:"public.by_size", id:"w1", expect_init: { value: [
    { k:"b", n:"3" },
    { k:"a", n:"1" }
  ] } } ]
}"##;

#[test]
fn control_sort_via_named_group_output() {
    assert_all_pass(&run(SORT_VIA_NAMED_OUTPUT));
}

// ── CONTROL: the same grouped view, no `$sort`, key-ascending default order ───
const GROUPED_DEFAULT_ORDER: &str = r##"{
  format: 1
  name: grouped-default-order
  suite: scenario
  spec: ["#views","§7.2","§B.5"]
  package: { $liasse:1, $app:"t.sortgroupdef@1.0.0", $model: {
    items: { $key:"id", id:"text", cat:"text" }
    $public: {
      by_size: { $view: ".items { $key: k, k: cat }" }
    }
  }, $data: { items: {
    a1:{cat:"a"}, b1:{cat:"b"}, b2:{cat:"b"}, b3:{cat:"b"}
  } } }
  steps: [ { watch:"public.by_size", id:"w1", expect_init: { value: [
    { k:"a" },
    { k:"b" }
  ] } } ]
}"##;

#[test]
fn control_grouped_default_order() {
    assert_all_pass(&run(GROUPED_DEFAULT_ORDER));
}
