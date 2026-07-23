//! RED-TEAM finding (§7.3 / Annex B.5 — the sorted-view identity tiebreak must be
//! key VALUE order, not canonical key TEXT order).
//!
//! # What the SPEC pins
//!
//! Annex B (intro): "Every sortable Liasse value has a deterministic ascending
//! total order. … Sort keys compare lexicographically from left to right; row
//! identity and then occurrence identity complete the order where applicable."
//! §B.5: "A collection defaults to key ascending. A view follows its declared
//! `$sort`, then inherited or synthetic row identity, then occurrence identity."
//! §B.1: `int` ascending order is "mathematical integer order".
//!
//! So when a view's declared `$sort` keys TIE, the next tiebreak is the row's
//! identity — its `$key` — ordered by the key's VALUE order (§B.1/§B.4), exactly
//! as the default (unsorted) collection order is "key ascending". For an `int`
//! key that is mathematical order: 2 < 3 < 10. The canonical key TEXT (Annex D.2)
//! of these ints is `"2"`, `"3"`, `"10"`; a lexicographic TEXT comparison gives
//! the WRONG order `"10" < "2" < "3"` (i.e. 10, 2, 3).
//!
//! # The divergence (root cause, hand-traced)
//!
//! The evaluator sorts projected rows through `order_rows`
//! (`crates/liasse-expr/src/eval/views.rs`), whose final tiebreak is
//! `SortOrder::compare(a_keys, a_row.id(), …)` — it compares two rows' `RowId`s
//! (`crates/liasse-expr/src/order.rs`). A `RowId`'s key part is the canonical D.2
//! key TEXT (`RowIdPart::Key(String)`, `crates/liasse-expr/src/env.rs`), so the
//! tiebreak orders by TEXT, not by the key's value. For `int`/`decimal`/composite
//! keys, text order diverges from the mandated value order.
//!
//! The DEFAULT (unsorted) order is correct — it comes from the store's
//! `KeyValue` value order (`liasse-store`) — which is why the `control_*`
//! no-`$sort` case passes: the bug is confined to the sorted path's tiebreak.
//!
//! Every expectation is hand-derived from SPEC.md (§B.1/§B.5), asserted on
//! distinct TEXT `tag` fields so the wire form is unambiguous, and is the
//! REVERSE of the buggy text order — so the case is load-bearing and
//! non-tautological.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

fn run(app: &str) -> CaseResult {
    let case = Case::from_hjson(app, Path::new("<redteam-sort-tiebreak-numeric-key>"), &BTreeSet::new())
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
// All three rows tie on `grp = "x"`, so the sort falls through to row identity
// (the `int` key `id`) ascending: 2 < 3 < 10 (mathematical, §B.1) → tags
// two, three, ten. The buggy text tiebreak gives "10" < "2" < "3" → ten, two,
// three, the order this case rejects.
const TIE_TO_INT_KEY: &str = r##"{
  format: 1
  name: sort-tiebreak-int-key-value-order
  suite: scenario
  spec: ["#views","§7.3","§B.5","§B.1"]
  package: { $liasse:1, $app:"t.tieint@1.0.0", $model: {
    items: { $key:"id", id:"int", grp:"text", tag:"text" }
    $public: {
      bygrp: { $view: ".items { tag, grp, $sort: [grp] }" }
    }
  }, $data: { items: {
    "2":  { grp:"x", tag:"two" },
    "10": { grp:"x", tag:"ten" },
    "3":  { grp:"x", tag:"three" }
  } } }
  steps: [ { watch:"public.bygrp", id:"w1", expect_init: { value: [
    { tag:"two",   grp:"x" },
    { tag:"three", grp:"x" },
    { tag:"ten",   grp:"x" }
  ] } } ]
}"##;

#[test]
fn tie_falls_to_int_key_value_order() {
    assert_all_pass(&run(TIE_TO_INT_KEY));
}

// ── CONTROL: no `$sort`, the DEFAULT key-ascending order is already correct ───
// Proves the store/default path orders int keys by value (2, 3, 10) and isolates
// the fault to the sorted-view tiebreak.
const DEFAULT_INT_KEY_ORDER: &str = r##"{
  format: 1
  name: default-int-key-order
  suite: scenario
  spec: ["#views","§B.5","§B.1"]
  package: { $liasse:1, $app:"t.defint@1.0.0", $model: {
    items: { $key:"id", id:"int", tag:"text" }
    $public: {
      all: { $view: ".items { tag }" }
    }
  }, $data: { items: {
    "2":  { tag:"two" },
    "10": { tag:"ten" },
    "3":  { tag:"three" }
  } } }
  steps: [ { watch:"public.all", id:"w1", expect_init: { value: [
    { tag:"two" },
    { tag:"three" },
    { tag:"ten" }
  ] } } ]
}"##;

#[test]
fn control_default_int_key_order() {
    assert_all_pass(&run(DEFAULT_INT_KEY_ORDER));
}

// ── ADJACENT: a GROUPED view, tie on the aggregate, tiebreak by synthetic int
// `$key`. Each account groups one line, so every group's `total` is 5 — a full
// tie on the declared `$sort: [total]`. The tiebreak is the synthetic key
// `acct` value ascending: 2 < 3 < 10 → tags two, three, ten.
const GROUPED_TIE_TO_INT_SYNTHETIC_KEY: &str = r##"{
  format: 1
  name: grouped-sort-tiebreak-int-synthetic-key
  suite: scenario
  spec: ["#views","§7.2","§7.3","§7.5","§B.5","§B.1"]
  package: { $liasse:1, $app:"t.tiegrp@1.0.0", $model: {
    lines: { $key:"id", id:"text", acct:"int", amt:"int", tag:"text" }
    $public: {
      totals: { $view: ".lines { $key: acct, acct, total: sum(group.amt), tag: min(group.tag), $sort: [\"total\"] }" }
    }
  }, $data: { lines: {
    l1: { acct:"2",  amt:"5", tag:"two" },
    l2: { acct:"10", amt:"5", tag:"ten" },
    l3: { acct:"3",  amt:"5", tag:"three" }
  } } }
  steps: [ { watch:"public.totals", id:"w1", expect_init: { value: [
    { total:"5", tag:"two",   "...": true },
    { total:"5", tag:"three", "...": true },
    { total:"5", tag:"ten",   "...": true }
  ] } } ]
}"##;

#[test]
fn grouped_tie_falls_to_int_synthetic_key_value_order() {
    assert_all_pass(&run(GROUPED_TIE_TO_INT_SYNTHETIC_KEY));
}

// ── ADJACENT: a COMPOSITE key whose second component is `int`. Rows tie on the
// declared sort key `grp`; the tiebreak is the composite key value order
// [region, seq] (§B.4), each component in its own type order (§B.1): within
// region "eu", seq 2 < 3 < 10. Text-join order would give "eu:10" < "eu:2".
const COMPOSITE_INT_COMPONENT_TIEBREAK: &str = r##"{
  format: 1
  name: composite-int-component-tiebreak
  suite: scenario
  spec: ["#views","§7.3","§B.5","§B.4","§B.1"]
  package: { $liasse:1, $app:"t.tiecomp@1.0.0", $model: {
    slots: { $key:["region","seq"], region:"text", seq:"int", grp:"text", tag:"text" }
    $public: {
      bygrp: { $view: ".slots { tag, grp, $sort: [grp] }" }
    }
  }, $data: { slots: {
    "eu:2":  { grp:"x", tag:"two" },
    "eu:10": { grp:"x", tag:"ten" },
    "eu:3":  { grp:"x", tag:"three" }
  } } }
  steps: [ { watch:"public.bygrp", id:"w1", expect_init: { value: [
    { tag:"two",   grp:"x" },
    { tag:"three", grp:"x" },
    { tag:"ten",   grp:"x" }
  ] } } ]
}"##;

#[test]
fn composite_int_component_tiebreak_value_order() {
    assert_all_pass(&run(COMPOSITE_INT_COMPONENT_TIEBREAK));
}
