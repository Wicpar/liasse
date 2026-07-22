//! The verdict and reporting model, plus the glue that scores an
//! [`Observation`] against an [`Expect`].
//!
//! Three layers: [`check_expectation`] judges one expectation and yields a
//! per-expectation [`Verdict`]; [`CaseResult`] rolls a case's [`StepTrace`]s up
//! into a per-case [`CaseVerdict`] (pass / fail / skip / unspecified-
//! observations); and [`ConformanceSummary`] aggregates a whole [`Report`] by
//! area and suite — the conformance report a runner prints.

use std::collections::BTreeMap;

use crate::contract::Observation;
use crate::corpus::{Area, SuiteKind};
use crate::expect::Expect;
use crate::matcher::{Bindings, Matcher};
use crate::outcome::Outcome;
use crate::trace::{StepResult, StepTrace};

/// The result of checking one expectation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The observation satisfied the expectation.
    Pass,
    /// The observation contradicted the expectation.
    Fail {
        /// Why the check failed.
        reason: String,
    },
    /// The expectation could not be evaluated (e.g. an unsupported step).
    Skipped {
        /// Why the check was skipped.
        reason: String,
    },
}

impl Verdict {
    /// Whether this verdict is a pass.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// The overall verdict for one whole case, aggregated from its step traces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaseVerdict {
    /// Every step passed and none were left unspecified.
    Pass,
    /// At least one step failed.
    Fail {
        /// How many steps failed.
        failures: usize,
    },
    /// The case could not be run to a judgement (e.g. a driver/transport error).
    Skipped {
        /// Why the case was skipped.
        reason: String,
    },
    /// The case ran to completion with recorded-but-unjudged `unspecified`
    /// steps and no failures.
    UnspecifiedObservations {
        /// How many steps were recorded without judgement.
        count: usize,
    },
}

impl CaseVerdict {
    /// Aggregate a case verdict from its step traces. Precedence, most severe
    /// first: any failure ⇒ `Fail`; else any skip ⇒ `Skipped`; else any
    /// unspecified observation ⇒ `UnspecifiedObservations`; else `Pass`.
    #[must_use]
    pub fn from_steps(steps: &[StepTrace]) -> Self {
        let failures = steps.iter().filter(|s| s.result.is_fail()).count();
        if failures > 0 {
            return Self::Fail { failures };
        }
        if let Some(reason) = steps.iter().find_map(|s| match &s.result {
            StepResult::Skipped { reason } => Some(reason.clone()),
            _ => None,
        }) {
            return Self::Skipped { reason };
        }
        let count = steps.iter().filter(|s| matches!(s.result, StepResult::Unspecified { .. })).count();
        if count > 0 {
            return Self::UnspecifiedObservations { count };
        }
        Self::Pass
    }

    /// Whether this verdict is a clean pass.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// The verdict for one whole case, with its per-step trace.
#[derive(Debug, Clone)]
pub struct CaseResult {
    /// The case's chapter.
    pub area: Area,
    /// The case's suite class.
    pub suite_kind: SuiteKind,
    /// The case name.
    pub name: String,
    /// The overall verdict.
    pub verdict: CaseVerdict,
    /// The per-step traces in run order.
    pub steps: Vec<StepTrace>,
}

/// A run's accumulated case results.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Per-case results in run order.
    pub results: Vec<CaseResult>,
}

impl Report {
    /// An empty report.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one case result.
    pub fn record(&mut self, result: CaseResult) {
        self.results.push(result);
    }

    /// Count of cleanly passing cases.
    #[must_use]
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|r| r.verdict.is_pass()).count()
    }

    /// Count of failing cases.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.results.iter().filter(|r| matches!(r.verdict, CaseVerdict::Fail { .. })).count()
    }

    /// Whether every recorded case passed cleanly.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|r| r.verdict.is_pass())
    }

    /// Aggregate the whole report by area and suite.
    #[must_use]
    pub fn summarize(&self) -> ConformanceSummary {
        let mut by_area: BTreeMap<String, AreaTally> = BTreeMap::new();
        let mut total = AreaTally::default();
        for result in &self.results {
            let tally = by_area.entry(result.area.as_str().to_owned()).or_default();
            tally.record(&result.verdict, result.suite_kind);
            total.record(&result.verdict, result.suite_kind);
        }
        ConformanceSummary { total, by_area }
    }
}

/// Running counts for one area (or the whole run).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AreaTally {
    /// Cleanly passing cases.
    pub passed: usize,
    /// Failing cases.
    pub failed: usize,
    /// Skipped cases.
    pub skipped: usize,
    /// Cases with recorded unspecified observations.
    pub unspecified: usize,
    /// Cases from the `common` suite.
    pub common: usize,
    /// Cases from the `red` suite.
    pub red: usize,
}

impl AreaTally {
    fn record(&mut self, verdict: &CaseVerdict, suite: SuiteKind) {
        match verdict {
            CaseVerdict::Pass => self.passed += 1,
            CaseVerdict::Fail { .. } => self.failed += 1,
            CaseVerdict::Skipped { .. } => self.skipped += 1,
            CaseVerdict::UnspecifiedObservations { .. } => self.unspecified += 1,
        }
        match suite {
            SuiteKind::Common => self.common += 1,
            SuiteKind::Red => self.red += 1,
        }
    }

    /// Total cases counted.
    #[must_use]
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.skipped + self.unspecified
    }
}

/// A whole-corpus conformance summary: an overall tally plus a per-area
/// breakdown, in deterministic area order.
#[derive(Debug, Clone)]
pub struct ConformanceSummary {
    /// The tally across every case.
    pub total: AreaTally,
    /// Per-area tallies, keyed by area name.
    pub by_area: BTreeMap<String, AreaTally>,
}

impl ConformanceSummary {
    /// Whether the run has no failing cases.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.total.failed == 0
    }
}

/// Score an observation against an `expect` block, threading `env` for
/// `$bind:`/`$ref:` matchers. An `unspecified` expectation is caught by the
/// engine before this runs; here it records a pass without judging the value.
#[must_use]
pub fn check_expectation(expect: &Expect, observed: &Observation, env: &mut Bindings) -> Verdict {
    let expected_outcome = expect.outcome.unwrap_or(Outcome::Ok);
    if expected_outcome == Outcome::Unspecified {
        return Verdict::Pass;
    }
    if observed.outcome != expected_outcome {
        return Verdict::Fail {
            reason: format!("expected outcome `{expected_outcome}`, observed `{}`", observed.outcome),
        };
    }
    if let Some(completion) = expect.completion
        && observed.completion != Some(completion)
    {
        return Verdict::Fail {
            reason: format!("expected completion `{completion}`, observed `{:?}`", observed.completion),
        };
    }
    let value_verdict = check_value(expect, observed, env);
    if !value_verdict.is_pass() {
        return value_verdict;
    }
    check_served_fetch(expect, observed)
}

/// §18.8: a `blob_get` step MAY assert the served `bytes` (the delivered
/// content) and the fetch-plan `holders` (the verified holders in `$serve`
/// order). Both sit in [`Expect::extra`] — outside the common value vocabulary,
/// carried per FORMAT.md as chapter-specific members — so without this check
/// they would be parsed and then silently ignored, letting a served-content or
/// serve-order mismatch pass vacuously. When present, each is compared exactly
/// against the fetch observation's recorded member (`holders` is order-sensitive
/// by construction). Only these two members are consulted, and only a `blob_get`
/// fetch records them, so no other step kind is affected.
fn check_served_fetch(expect: &Expect, observed: &Observation) -> Verdict {
    for member in ["bytes", "holders"] {
        let Some(expected) = expect.extra.get(member) else { continue };
        match observed.extra.get(member) {
            Some(actual) if actual == expected => {}
            Some(actual) => {
                return Verdict::Fail {
                    reason: format!("blob fetch `{member}` mismatch: expected {expected}, observed {actual}"),
                };
            }
            None => {
                return Verdict::Fail {
                    reason: format!("expected blob fetch `{member}` {expected}, none observed"),
                };
            }
        }
    }
    Verdict::Pass
}

fn check_value(expect: &Expect, observed: &Observation, env: &mut Bindings) -> Verdict {
    if let Some(matcher) = &expect.value {
        // A top-level `value: "$absent"` asserts the action produced no value — a
        // response-free mutation (§13.8, omission of `$return`), which passes exactly
        // when no response value is observed (as an object *member* `$absent` still
        // means "this member is absent", handled inside `Matcher::check`).
        if matches!(matcher, Matcher::Absent) {
            return match &observed.value {
                None => Verdict::Pass,
                Some(_) => Verdict::Fail { reason: "expected no response value, one observed".to_owned() },
            };
        }
        return match &observed.value {
            Some(value) => match matcher.check(value, env) {
                Ok(()) => Verdict::Pass,
                Err(err) => Verdict::Fail { reason: format!("value mismatch: {err}") },
            },
            None => Verdict::Fail { reason: "expected a value, none observed".to_owned() },
        };
    }
    if let Some(alternatives) = &expect.one_of {
        return check_one_of(alternatives, observed, env);
    }
    Verdict::Pass
}

fn check_one_of(alternatives: &[Matcher], observed: &Observation, env: &mut Bindings) -> Verdict {
    let Some(value) = &observed.value else {
        return Verdict::Fail { reason: "expect_one_of requires an observed value".to_owned() };
    };
    for matcher in alternatives {
        let mut trial = env.clone();
        if matcher.check(value, &mut trial).is_ok() {
            *env = trial;
            return Verdict::Pass;
        }
    }
    Verdict::Fail { reason: "value matched none of the expect_one_of alternatives".to_owned() }
}
