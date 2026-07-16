//! The verdict and reporting model, plus the glue that scores an
//! [`Observation`] against an [`Expect`].

use crate::contract::Observation;
use crate::corpus::{Area, SuiteKind};
use crate::expect::Expect;
use crate::matcher::{Bindings, Matcher};
use crate::outcome::Outcome;

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

/// The verdict for one whole case.
#[derive(Debug, Clone)]
pub struct CaseResult {
    /// The case's chapter.
    pub area: Area,
    /// The case's suite class.
    pub suite_kind: SuiteKind,
    /// The case name.
    pub name: String,
    /// The overall verdict.
    pub verdict: Verdict,
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

    /// Count of passing cases.
    #[must_use]
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|r| r.verdict.is_pass()).count()
    }

    /// Count of failing cases.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.results.iter().filter(|r| matches!(r.verdict, Verdict::Fail { .. })).count()
    }

    /// Count of skipped cases.
    #[must_use]
    pub fn skipped(&self) -> usize {
        self.results.iter().filter(|r| matches!(r.verdict, Verdict::Skipped { .. })).count()
    }

    /// Whether every recorded case passed.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|r| r.verdict.is_pass())
    }
}

/// Score an observation against an `expect` block, threading `env` for
/// `$bind:`/`$ref:` matchers. An `unspecified` expectation records the
/// observation without judging its value.
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
    check_value(expect, observed, env)
}

fn check_value(expect: &Expect, observed: &Observation, env: &mut Bindings) -> Verdict {
    if let Some(matcher) = &expect.value {
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
