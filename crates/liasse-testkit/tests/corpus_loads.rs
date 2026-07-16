//! Centerpiece conformance test: the entire `tests/` corpus loads into typed
//! cases. This is a real external invariant — the corpus is authored to conform
//! to FORMAT.md, so every file must yield a valid [`Case`].

use liasse_testkit::{Case, CaseBody, Corpus, LoadError, LoadedCase, SuiteKind};

type TestResult = Result<(), String>;

fn results() -> Result<Vec<Result<LoadedCase, LoadError>>, String> {
    Corpus::load_results_from(&Corpus::default_root()).map_err(|e| e.to_string())
}

#[test]
fn every_corpus_file_loads() -> TestResult {
    let results = results()?;
    assert!(results.len() >= 800, "expected the full corpus, found {} files", results.len());

    let failures: Vec<String> = results.iter().filter_map(|r| r.as_ref().err().map(ToString::to_string)).collect();
    assert!(failures.is_empty(), "{} case(s) failed to load:\n{}", failures.len(), failures.join("\n"));
    Ok(())
}

#[test]
fn loaded_cases_conform_to_format_invariants() -> TestResult {
    // Every loaded case must satisfy the FORMAT.md outcome/`violates` coupling
    // with zero allowlisted deviations: the checker is strict and correct, and
    // the corpus is authored to conform.
    let mut violations = Vec::new();
    for result in results()? {
        let Ok(loaded) = result else { continue }; // reported by every_corpus_file_loads
        if let Some(reason) = loaded.case.conformance_error() {
            violations.push(format!("{}: {reason}", loaded.path.display()));
        }
    }
    assert!(violations.is_empty(), "{} conformance violation(s):\n{}", violations.len(), violations.join("\n"));
    Ok(())
}

#[test]
fn every_case_name_matches_its_filename() -> TestResult {
    let mut mismatches = Vec::new();
    for result in results()? {
        let Ok(loaded) = result else { continue };
        let stem = loaded.path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        if stem != loaded.case.name {
            mismatches.push(format!("{}: name `{}` != stem `{stem}`", loaded.path.display(), loaded.case.name));
        }
    }
    assert!(mismatches.is_empty(), "{} name mismatch(es):\n{}", mismatches.len(), mismatches.join("\n"));
    Ok(())
}

#[test]
fn both_suites_and_many_areas_are_present() -> TestResult {
    let cases = Corpus::load().map_err(|e| e.to_string())?.cases;
    let common = cases.iter().filter(|c| c.suite_kind == SuiteKind::Common).count();
    let red = cases.iter().filter(|c| c.suite_kind == SuiteKind::Red).count();
    assert!(common > 0 && red > 0, "expected both common and red cases, got {common}/{red}");

    let areas: std::collections::BTreeSet<_> = cases.iter().map(|c| c.area.as_str().to_owned()).collect();
    assert!(areas.len() >= 20, "expected many chapters, found {}", areas.len());

    let statics = cases.iter().filter(|c| matches!(c.case.body, CaseBody::Static(_))).count();
    let scenarios = cases.iter().filter(|c| matches!(c.case.body, CaseBody::Scenario(_))).count();
    assert!(statics > 0 && scenarios > 0, "expected both static and scenario cases, got {statics}/{scenarios}");
    Ok(())
}

/// A hand-picked case is tagged from its on-disk location, not self-reference.
#[test]
fn known_case_is_tagged_and_shaped_correctly() -> TestResult {
    let cases = Corpus::load().map_err(|e| e.to_string())?.cases;
    let found = cases.iter().find(|c| {
        c.area.as_str() == "05-state-model"
            && c.suite_kind == SuiteKind::Red
            && c.case.name == "decimal-key-numeric-equality-collision"
    });
    let case: &Case = found.map(|c| &c.case).ok_or("known 05-state-model red case is missing")?;
    assert!(!case.spec.is_empty(), "every case cites at least one spec anchor");
    Ok(())
}
