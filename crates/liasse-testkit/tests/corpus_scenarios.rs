//! The scenario conformance runner (memory store) — a **hard gate**.
//!
//! Loads every `scenario` case in the corpus, drives each through the real
//! runtime + surface stack via [`ScenarioAdapter`] over an in-memory store, and
//! writes a machine-readable per-case report to
//! `target/conformance/scenarios-memory.json`, plus prints the per-area
//! [`ConformanceSummary`].
//!
//! Unlike the earlier phase, the run *passing* is now the contract. Every case
//! must end one of three admissible ways ([`scenario_gate`]):
//!
//! - a clean `Pass`;
//! - `UnspecifiedObservations` — behaviour SPEC.md does not pin (`SPEC-ISSUES`);
//! - a `Fail`/`Skipped` verdict whose key is on the [`SKIP`] debt ledger.
//!
//! Any other case — a non-pass verdict absent from the ledger — fails the test.
//! Symmetrically a ledger entry that has started passing (or names no live case)
//! is stale and also fails the test, so the ledger can only shrink. Each case
//! runs inside `catch_unwind` so a panic becomes a `Skipped` verdict rather than
//! aborting the whole run.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;

use liasse_testkit::scenario_gate::{classify, key as gate_key, GateClass, SKIP};
use liasse_testkit::{
    run_loaded, CaseResult, CaseVerdict, ConformanceSummary, Corpus, LoadedCase, Report,
    ScenarioAdapter, Suite,
};
use serde_json::json;

/// Where the machine-readable report is written, relative to the crate.
fn report_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/conformance/scenarios-memory.json")
}

#[test]
fn scenarios_gate_against_memory_store() {
    let corpus = Corpus::load().expect("corpus loads");
    let scenarios: Vec<_> =
        corpus.cases.iter().filter(|loaded| loaded.case.suite == Suite::Scenario).collect();
    assert!(!scenarios.is_empty(), "the corpus must carry scenario cases");

    let mut report = Report::new();
    let mut entries = Vec::with_capacity(scenarios.len());
    for loaded in &scenarios {
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut adapter = ScenarioAdapter::build(&loaded.case);
            run_loaded(&mut adapter, loaded)
        }))
        .unwrap_or_else(|_| panicked_case(loaded));
        entries.push(report_entry(&result));
        report.record(result);
    }

    assert_eq!(report.results.len(), scenarios.len(), "every scenario case is judged");

    write_report(&entries);
    print_summary(&report.summarize());
    assert!(report_path().exists(), "the conformance report was written");

    enforce_gate(&report.results);
}

/// Fail the test unless every case is admissible and the ledger has no stale
/// entries. The two failure modes are reported together so a single run shows
/// the whole delta.
fn enforce_gate(results: &[CaseResult]) {
    let mut unacknowledged: Vec<String> = Vec::new();
    let mut acknowledged: BTreeSet<String> = BTreeSet::new();
    let mut pass = 0usize;
    let mut unspecified = 0usize;
    for result in results {
        let key = gate_key(result.area.as_str(), &result.name);
        match classify(&result.verdict, &key) {
            GateClass::Pass => pass += 1,
            GateClass::Unspecified => unspecified += 1,
            GateClass::AcknowledgedDebt(_) => {
                acknowledged.insert(key);
            }
            GateClass::Unacknowledged => {
                unacknowledged.push(format!("  {key}  ({})", verdict_token(&result.verdict)));
            }
        }
    }

    // A ledger entry is stale if no live case still holds it as acknowledged
    // debt (the case now passes, is unspecified, or no longer exists).
    let stale: Vec<&str> =
        SKIP.iter().map(|(k, _)| *k).filter(|k| !acknowledged.contains(*k)).collect();

    println!(
        "\nscenario gate (memory): {pass} pass, {unspecified} unspecified, {} acknowledged debt, \
         {} unacknowledged, {} stale ledger entries",
        acknowledged.len(),
        unacknowledged.len(),
        stale.len(),
    );

    let mut problems = String::new();
    if !unacknowledged.is_empty() {
        unacknowledged.sort();
        problems.push_str(&format!(
            "\n{} case(s) ended non-pass but are absent from the SKIP ledger — either fix them or \
             add them with a capability reason:\n{}\n",
            unacknowledged.len(),
            unacknowledged.join("\n"),
        ));
    }
    if !stale.is_empty() {
        problems.push_str(&format!(
            "\n{} SKIP ledger ent(y/ies) are stale (the case now passes, is unspecified, or does \
             not exist) — remove them so the ledger only shrinks:\n{}\n",
            stale.len(),
            stale.iter().map(|k| format!("  {k}")).collect::<Vec<_>>().join("\n"),
        ));
    }
    assert!(problems.is_empty(), "scenario gate failed:{problems}");
}

/// The report row for one case: identity, verdict, gate class, first divergence.
fn report_entry(result: &CaseResult) -> serde_json::Value {
    let key = gate_key(result.area.as_str(), &result.name);
    json!({
        "case": result.name,
        "area": result.area.as_str(),
        "suite": result.suite_kind.as_str(),
        "verdict": verdict_token(&result.verdict),
        "gate": gate_token(&classify(&result.verdict, &key)),
        "first_divergence": first_divergence(result),
    })
}

/// The bare verdict token for the report and summary.
fn verdict_token(verdict: &CaseVerdict) -> &'static str {
    match verdict {
        CaseVerdict::Pass => "pass",
        CaseVerdict::Fail { .. } => "fail",
        CaseVerdict::Skipped { .. } => "skipped",
        CaseVerdict::UnspecifiedObservations { .. } => "unspecified",
    }
}

/// The gate class as a report token.
fn gate_token(class: &GateClass) -> &'static str {
    match class {
        GateClass::Pass => "pass",
        GateClass::Unspecified => "unspecified",
        GateClass::AcknowledgedDebt(_) => "acknowledged-debt",
        GateClass::Unacknowledged => "unacknowledged",
    }
}

/// The first step that failed or was skipped — the earliest point the run
/// diverged from the case's expectation. `null` for a clean pass.
fn first_divergence(result: &CaseResult) -> serde_json::Value {
    use liasse_testkit::StepResult;
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

/// A synthetic result for a case whose run panicked: recorded as skipped so the
/// run completes and the panic is visible in the report.
fn panicked_case(loaded: &LoadedCase) -> CaseResult {
    CaseResult {
        area: loaded.area.clone(),
        suite_kind: loaded.suite_kind,
        name: loaded.case.name.clone(),
        verdict: CaseVerdict::Skipped { reason: "the stack panicked while running this case".to_owned() },
        steps: Vec::new(),
    }
}

/// Write the per-case report as pretty JSON, creating the output directory.
fn write_report(entries: &[serde_json::Value]) {
    let path = report_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create report directory");
    }
    let document = serde_json::to_string_pretty(&json!(entries)).expect("serialize report");
    std::fs::write(&path, document).expect("write report");
}

/// Print the per-area conformance tally, then the overall line.
fn print_summary(summary: &ConformanceSummary) {
    println!("\nscenario conformance (memory store)");
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
