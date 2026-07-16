//! The scenario conformance runner (PostgreSQL store) — a **hard gate**, plus a
//! store-contract **divergence check** against the in-memory reference.
//!
//! This drives the *identical* file-based scenario corpus that
//! `liasse-testkit`'s `corpus_scenarios` runs against the in-memory store, but
//! over `liasse-pg`'s [`PgStore`], provisioned per case through the same
//! self-provisioning [`support`] cluster the store-contract battery uses. It
//! gates the PostgreSQL run against the *same* [`SKIP`] ledger (via
//! [`scenario_gate`]) — so a case must `Pass`, be `UnspecifiedObservations`, or
//! be acknowledged debt, exactly as on memory.
//!
//! On top of the gate it runs each case through *both* backends and compares
//! them step by step. The store contract promises the two backends make the
//! runtime observe the same thing; any divergence in a case's per-step outcomes
//! or overall verdict is therefore a store-contract bug, reported prominently and
//! failing the test — it is never skip-listed away.
//!
//! The report is written to `target/conformance/scenarios-pg.json`. Each case's
//! throwaway schema is dropped at the end of the run (and the disposable cluster,
//! if one was bootstrapped, is torn down when the last handle drops).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;

use liasse_ident::InstanceId;
use liasse_pg::{PgStore, PgStoreFactory};
use liasse_testkit::scenario_gate::{classify, key as gate_key, GateClass};
use liasse_testkit::{
    run_loaded, CaseResult, CaseVerdict, ConformanceSummary, Corpus, LoadedCase, MemoryProvision,
    Report, ScenarioAdapter, StepResult, StepTrace, StoreProvision, Suite,
};
use serde_json::json;

/// Where the PostgreSQL scenario report is written, relative to the crate.
fn report_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/conformance/scenarios-pg.json")
}

/// A [`StoreProvision`] over one self-provisioning PostgreSQL factory: each case
/// gets a fresh per-instance schema, and every schema created over the run is
/// remembered so it can be dropped when the run ends.
struct PgProvision {
    factory: PgStoreFactory,
    created: Vec<InstanceId>,
    seen: BTreeSet<String>,
}

impl PgProvision {
    fn new(factory: PgStoreFactory) -> Self {
        Self { factory, created: Vec::new(), seen: BTreeSet::new() }
    }

    /// Drop every schema this provisioner created — best-effort teardown so a
    /// developer-local PostgreSQL is left as clean as a disposable cluster.
    fn cleanup(&self) {
        for instance in &self.created {
            let _ = self.factory.drop_instance(instance);
        }
    }
}

impl StoreProvision for PgProvision {
    type Store = PgStore;

    fn provision(&mut self, instance: InstanceId) -> Result<Self::Store, String> {
        use liasse_store::StoreFactory;
        if self.seen.insert(instance.as_str().to_owned()) {
            self.created.push(instance.clone());
        }
        // `create` drops and recreates the instance's schema, so the up-to-three
        // load attempts a single case makes each start from a clean slate.
        self.factory.create(instance).map_err(|error| error.to_string())
    }
}

#[test]
fn scenarios_gate_against_pg_store() {
    let corpus = Corpus::load().expect("corpus loads");
    let scenarios: Vec<_> =
        corpus.cases.iter().filter(|loaded| loaded.case.suite == Suite::Scenario).collect();
    assert!(!scenarios.is_empty(), "the corpus must carry scenario cases");

    let handle = support::acquire();
    let mut provision = PgProvision::new(handle.factory("scen"));

    let mut report = Report::new();
    let mut entries = Vec::with_capacity(scenarios.len());
    let mut divergences: Vec<String> = Vec::new();

    for loaded in &scenarios {
        let memory = run_memory(loaded);
        let pg = run_pg(&mut provision, loaded);

        if let Some(detail) = diverges(&memory, &pg) {
            divergences.push(format!("  {}/{}: {detail}", loaded.area.as_str(), loaded.case.name));
        }
        entries.push(report_entry(&memory, &pg, diverges(&memory, &pg).is_some()));
        report.record(pg);
    }

    // Teardown before any assertion can unwind, so schemas are dropped even on a
    // gate failure. The `PgHandle` still tears the cluster down on its own drop.
    provision.cleanup();

    assert_eq!(report.results.len(), scenarios.len(), "every scenario case is judged");
    write_report(&entries);
    print_summary(&report.summarize());
    assert!(report_path().exists(), "the conformance report was written");

    enforce(&report.results, &divergences);
}

/// Drive `loaded` through the in-memory reference store.
fn run_memory(loaded: &LoadedCase) -> CaseResult {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut adapter = ScenarioAdapter::build_with(&mut MemoryProvision, &loaded.case);
        run_loaded(&mut adapter, loaded)
    }))
    .unwrap_or_else(|_| panicked_case(loaded))
}

/// Drive `loaded` through the PostgreSQL store.
fn run_pg(provision: &mut PgProvision, loaded: &LoadedCase) -> CaseResult {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut adapter = ScenarioAdapter::build_with(provision, &loaded.case);
        run_loaded(&mut adapter, loaded)
    }))
    .unwrap_or_else(|_| panicked_case(loaded))
}

/// Whether the two backends diverged on a case, and how. The store contract
/// promises the runtime observes the same thing over either backend, so a
/// difference in the overall verdict class or in any step's judged kind and
/// observed outcome is a store-contract bug. Skip *reasons* are not compared —
/// an adapter-level "unsupported step" skip is store-independent by construction.
fn diverges(memory: &CaseResult, pg: &CaseResult) -> Option<String> {
    if verdict_token(&memory.verdict) != verdict_token(&pg.verdict) {
        return Some(format!(
            "verdict `{}` (memory) vs `{}` (pg)",
            verdict_token(&memory.verdict),
            verdict_token(&pg.verdict)
        ));
    }
    if memory.steps.len() != pg.steps.len() {
        return Some(format!(
            "{} step(s) on memory vs {} on pg",
            memory.steps.len(),
            pg.steps.len()
        ));
    }
    for (m, p) in memory.steps.iter().zip(&pg.steps) {
        if step_sig(m) != step_sig(p) {
            return Some(format!(
                "step {} (`{}`): memory `{}` vs pg `{}`",
                m.index,
                m.action,
                step_sig(m),
                step_sig(p)
            ));
        }
    }
    None
}

/// A store-relevant per-step signature: the judged kind plus the spec outcome the
/// store made the runtime observe.
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

/// Fail the test on any store-contract divergence or any unacknowledged non-pass
/// PostgreSQL case. Divergences are reported first and loudest.
fn enforce(pg_results: &[CaseResult], divergences: &[String]) {
    let mut unacknowledged: Vec<String> = Vec::new();
    let (mut pass, mut unspecified, mut debt) = (0usize, 0usize, 0usize);
    for result in pg_results {
        let key = gate_key(result.area.as_str(), &result.name);
        match classify(&result.verdict, &key) {
            GateClass::Pass => pass += 1,
            GateClass::Unspecified => unspecified += 1,
            GateClass::AcknowledgedDebt(_) => debt += 1,
            GateClass::Unacknowledged => {
                unacknowledged.push(format!("  {key}  ({})", verdict_token(&result.verdict)));
            }
        }
    }

    println!(
        "\nscenario gate (pg): {pass} pass, {unspecified} unspecified, {debt} acknowledged debt, \
         {} unacknowledged, {} store-contract divergences vs memory",
        unacknowledged.len(),
        divergences.len(),
    );

    let mut problems = String::new();
    if !divergences.is_empty() {
        let mut sorted = divergences.to_vec();
        sorted.sort();
        problems.push_str(&format!(
            "\n>>> {} STORE-CONTRACT DIVERGENCE(S): PostgreSQL and the in-memory reference judged \
             the same case differently. This is a store bug — fix the backend, do not skip-list \
             it:\n{}\n",
            sorted.len(),
            sorted.join("\n"),
        ));
    }
    if !unacknowledged.is_empty() {
        unacknowledged.sort();
        problems.push_str(&format!(
            "\n{} pg case(s) ended non-pass but are absent from the SKIP ledger:\n{}\n",
            unacknowledged.len(),
            unacknowledged.join("\n"),
        ));
    }
    assert!(problems.is_empty(), "scenario pg gate failed:{problems}");
}

/// The per-case report row: both verdicts, the pg gate class, and — for a
/// divergence or a pg non-pass — the pg first-divergence detail.
fn report_entry(memory: &CaseResult, pg: &CaseResult, diverged: bool) -> serde_json::Value {
    let key = gate_key(pg.area.as_str(), &pg.name);
    json!({
        "case": pg.name,
        "area": pg.area.as_str(),
        "suite": pg.suite_kind.as_str(),
        "memory_verdict": verdict_token(&memory.verdict),
        "pg_verdict": verdict_token(&pg.verdict),
        "diverged": diverged,
        "gate": gate_token(&classify(&pg.verdict, &key)),
        "first_divergence": first_divergence(pg),
    })
}

fn verdict_token(verdict: &CaseVerdict) -> &'static str {
    match verdict {
        CaseVerdict::Pass => "pass",
        CaseVerdict::Fail { .. } => "fail",
        CaseVerdict::Skipped { .. } => "skipped",
        CaseVerdict::UnspecifiedObservations { .. } => "unspecified",
    }
}

fn gate_token(class: &GateClass) -> &'static str {
    match class {
        GateClass::Pass => "pass",
        GateClass::Unspecified => "unspecified",
        GateClass::AcknowledgedDebt(_) => "acknowledged-debt",
        GateClass::Unacknowledged => "unacknowledged",
    }
}

fn first_divergence(result: &CaseResult) -> serde_json::Value {
    for step in &result.steps {
        let reason = match &step.result {
            StepResult::Fail { reason } => Some(("fail", reason.clone())),
            StepResult::Skipped { reason } => Some(("skipped", reason.clone())),
            _ => None,
        };
        if let Some((kind, reason)) = reason {
            return json!({ "step": step.index, "action": step.action, "kind": kind, "reason": reason });
        }
    }
    serde_json::Value::Null
}

fn panicked_case(loaded: &LoadedCase) -> CaseResult {
    CaseResult {
        area: loaded.area.clone(),
        suite_kind: loaded.suite_kind,
        name: loaded.case.name.clone(),
        verdict: CaseVerdict::Skipped { reason: "the stack panicked while running this case".to_owned() },
        steps: Vec::new(),
    }
}

fn write_report(entries: &[serde_json::Value]) {
    let path = report_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create report directory");
    }
    let document = serde_json::to_string_pretty(&json!(entries)).expect("serialize report");
    std::fs::write(&path, document).expect("write report");
}

fn print_summary(summary: &ConformanceSummary) {
    println!("\nscenario conformance (pg store)");
    println!("{:<26} {:>5} {:>5} {:>5} {:>6} {:>6}", "area", "pass", "fail", "skip", "unspec", "total");
    for (area, tally) in &summary.by_area {
        println!(
            "{:<26} {:>5} {:>5} {:>5} {:>6} {:>6}",
            area, tally.passed, tally.failed, tally.skipped, tally.unspecified, tally.total()
        );
    }
    let total = &summary.total;
    println!(
        "{:<26} {:>5} {:>5} {:>5} {:>6} {:>6}",
        "TOTAL", total.passed, total.failed, total.skipped, total.unspecified, total.total()
    );
}
