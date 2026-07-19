//! RED-TEAM: export/restore of a top-level collection carrying a §5.3 STATIC
//! STRUCT, driven through BOTH store backends and checked for a memory-vs-pg
//! divergence (SPEC-ISSUES item 32: a backend disagreement is always a fix).
//!
//! The existing pg export/restore coverage (`composite-keyed-state-round-trips-
//! through-restore` in the corpus) carries composite keys and refs but NEVER a
//! static struct in a collection row. The portable codec serializes a static
//! struct member into the state section and reconstructs it via
//! `StateSection::row_type` (`fields.chain(structs)`) + the optional-wrapped
//! decode struct; on the pg side the row VALUE (struct included) round-trips
//! through the `value_codec` jsonb column and is rebuilt on the sandbox reopen.
//! This drives that whole path — capture, serialize, restore into a FRESH pg
//! sandbox instance, reload — over a struct with an omitted-optional member, a
//! scale-bearing decimal, and (second case) a nested set, and asserts pg observes
//! exactly what the in-memory reference does, step for step.
//!
//! Both scenarios pass on memory (see the liasse-testkit standalone probes); the
//! store contract requires pg to agree. Any per-step or verdict difference is a
//! store-contract bug reported here, never skip-listed.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::collections::{BTreeSet, BTreeMap};
use std::panic::AssertUnwindSafe;

use liasse_ident::InstanceId;
use liasse_pg::{PgStore, PgStoreFactory};
use liasse_store::StoreFactory;
use liasse_testkit::{
    run_case, Area, Case, CaseResult, MemoryProvision, ScenarioAdapter, StepResult, StepTrace,
    StoreProvision, SuiteKind,
};

/// A per-case fresh-schema pg provision, mirroring the corpus pg runner.
struct PgProvision {
    factory: PgStoreFactory,
    created: Vec<InstanceId>,
    seen: BTreeSet<String>,
}

impl PgProvision {
    fn new(factory: PgStoreFactory) -> Self {
        Self { factory, created: Vec::new(), seen: BTreeSet::new() }
    }
    fn cleanup(&self) {
        for instance in &self.created {
            let _ = self.factory.drop_instance(instance);
        }
    }
}

impl StoreProvision for PgProvision {
    type Store = PgStore;
    fn provision(&mut self, instance: InstanceId) -> Result<Self::Store, String> {
        if self.seen.insert(instance.as_str().to_owned()) {
            self.created.push(instance.clone());
        }
        self.factory.create(instance).map_err(|error| error.to_string())
    }
}

const SCALAR_STRUCT_APP: &str = r##"{
  format: 1
  name: static-struct-export-restore-pg
  suite: scenario
  spec: ["#history", "§19.10", "§19.2", "§5.3"]
  package: {
    $liasse: 1
    $app: "t.pg.sstruct@1.0.0"
    $model: {
      orders: {
        $key: "id"
        id: "text"
        address: {
          line1: "text"
          line2: "text?"
          city: "text"
          country: "text = 'FR'"
          tax: "decimal"
        }
      }
      notes: { $key: "id", id: "text", body: "text" }
      $public: {
        orders: { $view: ".orders { id, address, $sort: [id] }" }
        notes: { $view: ".notes { id, body, $sort: [id] }" }
      }
    }
    $data: {
      orders: {
        o1: { address: { line1: "1 Main St", line2: "Apt 4", city: "Paris", country: "FR", tax: "1.50" } }
        o2: { address: { line1: "9 Rue X", city: "Lyon", country: "US", tax: "2.00" } }
      }
      notes: { n1: { body: "hello" } }
    }
  }
  steps: [
    { export: { as: "a1" }, expect: { outcome: ok } }
    { in_sandbox: "s1", steps: [
      { restore: { from: "a1" }, expect: { outcome: ok } }
      { watch: "public.orders", id: "wo", expect_init: { value: [
        { id: "o1", address: { line1: "1 Main St", line2: "Apt 4", city: "Paris", country: "FR", tax: "1.5" } }
        { id: "o2", address: { line1: "9 Rue X", city: "Lyon", country: "US", tax: "2" } }
      ] } }
      { watch: "public.notes", id: "wn", expect_init: { value: [ { id: "n1", body: "hello" } ] } }
    ] }
  ]
}"##;

const SET_STRUCT_APP: &str = r##"{
  format: 1
  name: static-struct-set-export-restore-pg
  suite: scenario
  spec: ["#history", "§19.10", "§19.2", "§5.3", "§5.5"]
  package: {
    $liasse: 1
    $app: "t.pg.sstructset@1.0.0"
    $model: {
      docs: {
        $key: "id"
        id: "text"
        meta: { title: "text", tags: { $set: "text" } }
      }
      $public: { docs: { $view: ".docs { id, meta, $sort: [id] }" } }
    }
    $data: {
      docs: {
        d1: { meta: { title: "hello", tags: ["b", "a", "c"] } }
        d2: { meta: { title: "world", tags: [] } }
      }
    }
  }
  steps: [
    { export: { as: "a1" }, expect: { outcome: ok } }
    { in_sandbox: "s1", steps: [
      { restore: { from: "a1" }, expect: { outcome: ok } }
      { watch: "public.docs", id: "wd", expect_init: { value: [
        { id: "d1", meta: { title: "hello", tags: ["a", "b", "c"] } }
        { id: "d2", meta: { title: "world", tags: [] } }
      ] } }
    ] }
  ]
}"##;

fn parse(text: &str, name: &str) -> Case {
    let allowed: BTreeSet<String> =
        ["export", "in_sandbox", "restore"].into_iter().map(String::from).collect();
    Case::from_hjson(text, std::path::Path::new(name), &allowed).expect("case parses")
}

fn run_memory(case: &Case) -> CaseResult {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut adapter = ScenarioAdapter::build_with(&mut MemoryProvision, case);
        run_case(&mut adapter, &Area::new("19-history-artifacts"), SuiteKind::Red, case)
    }))
    .expect("memory run did not panic")
}

fn run_pg(provision: &mut PgProvision, case: &Case) -> CaseResult {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut adapter = ScenarioAdapter::build_with(provision, case);
        run_case(&mut adapter, &Area::new("19-history-artifacts"), SuiteKind::Red, case)
    }))
    .expect("pg run did not panic")
}

fn step_sig(step: &StepTrace) -> String {
    let kind = match &step.result {
        StepResult::Pass => "pass",
        StepResult::Fail { .. } => "fail",
        StepResult::Skipped { .. } => "skip",
        StepResult::Unspecified { .. } => "unspec",
    };
    let observed = step.observed.as_ref().map_or_else(String::new, ToString::to_string);
    format!("{kind}:{observed}")
}

/// Every step of a case must be a clean Pass — the case is authored so that the
/// spec-correct restore reproduces the seeded state; a non-pass is a real bug.
fn assert_case_passes(label: &str, result: &CaseResult) {
    for step in &result.steps {
        assert!(
            matches!(step.result, StepResult::Pass),
            "{label}: step {} (`{}`) did not pass: {} — {:?}",
            step.index,
            step.action,
            step_sig(step),
            step.result
        );
    }
}

/// Memory and pg must judge each step identically (store-contract parity).
fn assert_no_divergence(label: &str, memory: &CaseResult, pg: &CaseResult) {
    assert_eq!(
        memory.steps.len(),
        pg.steps.len(),
        "{label}: step count diverges: memory {} vs pg {}",
        memory.steps.len(),
        pg.steps.len()
    );
    let m: BTreeMap<usize, String> = memory.steps.iter().map(|s| (s.index, step_sig(s))).collect();
    let p: BTreeMap<usize, String> = pg.steps.iter().map(|s| (s.index, step_sig(s))).collect();
    assert_eq!(m, p, "{label}: memory-vs-pg per-step divergence (store-contract bug)");
}

#[test]
fn static_struct_export_restore_agrees_across_stores() {
    let handle = support::acquire();
    let mut provision = PgProvision::new(handle.factory("sstruct-xr"));

    for (label, text, name) in [
        ("scalar-struct", SCALAR_STRUCT_APP, "<static-struct-export-restore-pg>"),
        ("set-struct", SET_STRUCT_APP, "<static-struct-set-export-restore-pg>"),
    ] {
        let case = parse(text, name);
        let memory = run_memory(&case);
        let pg = run_pg(&mut provision, &case);

        // The reference must pass (externally-deducible spec behaviour), then pg
        // must agree with it step for step.
        assert_case_passes(&format!("{label} (memory)"), &memory);
        assert_no_divergence(label, &memory, &pg);
        assert_case_passes(&format!("{label} (pg)"), &pg);
    }

    provision.cleanup();
}
