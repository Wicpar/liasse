//! RED-TEAM: two runtime scenarios the corpus does not carry in this exact shape,
//! driven through BOTH store backends and checked for a memory-vs-pg divergence
//! (SPEC-ISSUES item 32: a backend disagreement is always a fix).
//!
//! 1. §15.6 funding view EXACT shape — a source projecting extra `price`
//!    metadata; the funding row must be exactly `{ source, pool, amount }` on
//!    either backend (existing corpus funding cases all use `"...": true`).
//! 2. §B.5/§B.1 combined sort — `$sort: [-opt, id]` over an optional decimal with
//!    none values and a `1.0 == 1.00` numeric tie; the row order must be identical
//!    over either backend (a store that ordered a decimal key/sort by scale/text,
//!    or placed the durable rows differently, would diverge on the scan the view
//!    reads).
//!
//! Both pass on memory (see the liasse-testkit standalone probes); the store
//! contract requires pg to agree, step for step.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;

use liasse_ident::InstanceId;
use liasse_pg::{PgStore, PgStoreFactory};
use liasse_store::StoreFactory;
use liasse_testkit::{
    run_case, Area, Case, CaseResult, MemoryProvision, ScenarioAdapter, StepResult, StepTrace,
    StoreProvision, SuiteKind,
};

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

const FUNDING_APP: &str = r##"{
  format: 1
  name: funding-view-exact-shape-pg
  suite: scenario
  spec: ["#meters", "§15.6", "§15.3"]
  package: {
    $liasse: 1
    $app: "t.pg.fundshape@1.0.0"
    $semantics: { timestamp_precision: "s" }
    $model: {
      users: {
        $key: "id"
        id: "text"
        topups: { $key: "id", id: "text", amount: "decimal", price: "decimal" }
        spends: {
          $key: "id"
          $consumes: "credits"
          id: "uuid = uuid()"
          amount: "decimal"
          occurred_at: "timestamp = now()"
        }
        $limits: {
          credits: {
            $sources: { topup: ".topups { $quantity: .amount, price }" }
            $order: ["price"]
          }
        }
        $mut: {
          consume: [ "spend = .spends + { amount: @amount }", "return spend { id, funding }" ]
        }
      }
      $public: {
        wallet: {
          $view: ".users { id, balance: .credits.balance }"
          $mut: { consume: ".users[@user].consume" }
        }
      }
    }
    $data: { users: { u1: { topups: { t1: { amount: "100", price: "9" } } } } }
  }
  steps: [
    { call: "public.wallet.consume", args: { user: "u1", amount: "40" },
      expect: { outcome: ok, value: {
        id: "$any:uuid"
        funding: [ { source: "topup", pool: "$any", amount: "40" } ]
      } } }
    { watch: "public.wallet", id: "w1", expect_init: { value: [ { id: "u1", balance: "60" } ] } }
  ]
}"##;

const SORT_APP: &str = r##"{
  format: 1
  name: sort-none-desc-decimal-tie-pg
  suite: scenario
  spec: ["#annex-b", "§B.5", "§B.1"]
  package: {
    $liasse: 1
    $app: "t.pg.nonedesctie@1.0.0"
    $model: {
      rows: { $key: "id", id: "text", opt: "decimal?" }
      $public: { ranked: { $view: ".rows { id, $sort: [-opt, id] }" } }
    }
    $data: {
      rows: {
        k1: {}
        k2: { opt: "1.00" }
        k3: { opt: "10" }
        k4: {}
        k5: { opt: "2" }
        k6: { opt: "1.0" }
      }
    }
  }
  steps: [
    { watch: "public.ranked", id: "w1", expect_init: { value: [
      { id: "k1" }, { id: "k4" }, { id: "k3" }, { id: "k5" }, { id: "k2" }, { id: "k6" }
    ] } }
  ]
}"##;

fn parse(text: &str, name: &str) -> Case {
    Case::from_hjson(text, std::path::Path::new(name), &BTreeSet::new()).expect("case parses")
}

fn run_with<P: StoreProvision<Store = S>, S: liasse_store::InstanceStore>(
    provision: &mut P,
    case: &Case,
    area: &str,
) -> CaseResult {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut adapter = ScenarioAdapter::build_with(provision, case);
        run_case(&mut adapter, &Area::new(area), SuiteKind::Red, case)
    }))
    .expect("run did not panic")
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

fn assert_no_divergence(label: &str, memory: &CaseResult, pg: &CaseResult) {
    assert_eq!(memory.steps.len(), pg.steps.len(), "{label}: step count diverges");
    let m: BTreeMap<usize, String> = memory.steps.iter().map(|s| (s.index, step_sig(s))).collect();
    let p: BTreeMap<usize, String> = pg.steps.iter().map(|s| (s.index, step_sig(s))).collect();
    assert_eq!(m, p, "{label}: memory-vs-pg per-step divergence (store-contract bug)");
}

#[test]
fn funding_shape_and_sort_tie_agree_across_stores() {
    let handle = support::acquire();
    let mut provision = PgProvision::new(handle.factory("fund-sort"));

    for (label, text, name, area) in [
        ("funding-shape", FUNDING_APP, "<funding-view-exact-shape-pg>", "15-meters"),
        ("sort-tie", SORT_APP, "<sort-none-desc-decimal-tie-pg>", "annex-b-total-order"),
    ] {
        let case = parse(text, name);
        let mut mem_provision = MemoryProvision;
        let memory = run_with(&mut mem_provision, &case, area);
        let pg = run_with(&mut provision, &case, area);

        assert_case_passes(&format!("{label} (memory)"), &memory);
        assert_no_divergence(label, &memory, &pg);
        assert_case_passes(&format!("{label} (pg)"), &pg);
    }

    provision.cleanup();
}
