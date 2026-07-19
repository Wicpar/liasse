//! RED-TEAM (§7.2 synthetic-`$key` grouping): a grouped `$view` whose synthetic
//! `$key` output is DERIVED FROM A ROW BINDING introduced by the view's own
//! source chain — a `::` traversal level (§6.4/§7.2) or a `[:name |  …]` filter
//! (§6.4) — evaluates the key WITHOUT that binding in scope and FAULTS at read
//! time, even though the declaration loaded cleanly.
//!
//! # What the SPEC pins
//!
//! §7.1 lets a projection output be `binding.field` and read a row binding in
//! scope. §6.4: a `::` traversal binds each traversed collection to its own field
//! name (`.companies::employees` binds `companies`), and `[:name | …]` binds the
//! row under test. §7.2: "A projection MAY declare a synthetic `$key` for grouping
//! … Rows sharing the synthetic key form one group." The `$key` names an output
//! field; that output is an ordinary projection member and MAY therefore be
//! `companies.name` (§7.1) — grouping child rows by a parent attribute is the
//! canonical use of a synthetic grouping key. §7.5: `count(group)` / `sum(group.f)`
//! are the aggregates over the group's source-row view. §B.5: output rows appear
//! in synthetic-key ascending order.
//!
//! So `.companies::employees { $key: cname, cname: companies.name, headcount:
//! count(group) }` is a well-formed grouped view. With companies `c1="Acme"`
//! (employees e1,e2) and `c2="Beta"` (employee e3) it MUST yield exactly two
//! output rows — `{cname:"Acme", headcount:2}` then `{cname:"Beta", headcount:1}`
//! — every value hand-derived from the seed above, independent of implementation.
//!
//! # The divergence
//!
//! The evaluator computes a row's group key in `group_key`
//! (`crates/liasse-expr/src/eval/views.rs`): it pushes the source row as `.` but,
//! unlike `project_row` and `eval_keys` (which both bind `scope.binds`), it NEVER
//! binds the source-chain row bindings before evaluating the `$key` output. So the
//! key expression `companies.name` resolves an unbound `companies` and the engine
//! raises `unbound binding` — a read-time host fault on a view the loader accepted.
//! `#[grouped_by_traversal_binding]` and `#[grouped_by_filter_binding]` FAIL today;
//! `#[control_grouped_by_plain_field]` (a `$key` that reads the source row's own
//! field, needing no binding) PASSES, isolating the fault to the missing binding
//! context in `group_key`.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

fn run(app: &str) -> CaseResult {
    let case = Case::from_hjson(app, Path::new("<redteam-grouped-key-binding>"), &BTreeSet::new())
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

/// §7.2 grouping key derived from a `::` traversal binding (`companies.name`):
/// group employees by their company name. Expected two groups in key-ascending
/// order; currently faults with `unbound binding companies`.
const TRAVERSAL_BINDING: &str = r##"{
  format: 1
  name: grouped-key-from-traversal-binding
  suite: scenario
  spec: ["#views","§7.2","§7.1","§6.4","§7.5","§B.5"]
  package: { $liasse:1, $app:"t.groupkeybind@1.0.0", $model: {
    companies: { $key:"cid", cid:"text", name:"text",
      employees: { $key:"eid", eid:"text" } }
    $public: {
      by_company: { $view: ".companies::employees { $key: cname, cname: companies.name, headcount: count(group) }" }
    }
  }, $data: { companies: {
    c1: { name:"Acme", employees: { e1:{}, e2:{} } }
    c2: { name:"Beta", employees: { e3:{} } }
  } } }
  steps: [ { watch:"public.by_company", id:"w1", expect_init: { value: [
    { cname:"Acme", headcount:"2" },
    { cname:"Beta", headcount:"1" }
  ] } } ]
}"##;

#[test]
fn grouped_by_traversal_binding() {
    assert_all_pass(&run(TRAVERSAL_BINDING));
}

/// §7.2 grouping key derived from a `[:name | …]` filter binding (`it.cat`):
/// semantically identical to grouping by the row's own `.cat`, so it MUST yield
/// the same two groups. Currently faults with `unbound binding it`.
const FILTER_BINDING: &str = r##"{
  format: 1
  name: grouped-key-from-filter-binding
  suite: scenario
  spec: ["#views","§7.2","§7.1","§6.4","§7.5","§B.5"]
  package: { $liasse:1, $app:"t.groupkeybind2@1.0.0", $model: {
    items: { $key:"id", id:"text", cat:"text", amount:"int" }
    $public: {
      totals: { $view: ".items[:it | it.amount > 0] { $key: k, k: it.cat, total: sum(group.amount) }" }
    }
  }, $data: { items: {
    a:{cat:"x",amount:"1"}, b:{cat:"x",amount:"2"}, c:{cat:"y",amount:"5"}
  } } }
  steps: [ { watch:"public.totals", id:"w1", expect_init: { value: [
    { k:"x", total:"3" },
    { k:"y", total:"5" }
  ] } } ]
}"##;

#[test]
fn grouped_by_filter_binding() {
    assert_all_pass(&run(FILTER_BINDING));
}

/// PASSING CONTROL: the SAME grouping, but the synthetic `$key` reads the source
/// row's OWN field (`cat`) and so needs no source-chain binding. `group_key`
/// resolves it against the pushed `.` row, so grouping itself works — proving the
/// fault above is specifically the missing binding context, not grouping.
const CONTROL_PLAIN_FIELD: &str = r##"{
  format: 1
  name: grouped-key-plain-field-control
  suite: scenario
  spec: ["#views","§7.2","§7.5","§B.5"]
  package: { $liasse:1, $app:"t.groupkeyplain@1.0.0", $model: {
    items: { $key:"id", id:"text", cat:"text", amount:"int" }
    $public: {
      totals: { $view: ".items { $key: k, k: cat, total: sum(group.amount) }" }
    }
  }, $data: { items: {
    a:{cat:"x",amount:"1"}, b:{cat:"x",amount:"2"}, c:{cat:"y",amount:"5"}
  } } }
  steps: [ { watch:"public.totals", id:"w1", expect_init: { value: [
    { k:"x", total:"3" },
    { k:"y", total:"5" }
  ] } } ]
}"##;

#[test]
fn control_grouped_by_plain_field() {
    assert_all_pass(&run(CONTROL_PLAIN_FIELD));
}
