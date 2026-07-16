//! Per-step execution traces.
//!
//! The engine records one [`StepTrace`] per leaf step it runs, capturing the
//! observed spec outcome and how the step's expectation (if any) was judged. An
//! `outcome: unspecified` step is *recorded* ([`StepResult::Unspecified`]) but
//! never judged, per FORMAT.md; a step with no expectation whose action
//! succeeded is a [`StepResult::Pass`]. Trace quality is verdict quality: a
//! [`StepResult::Fail`] carries the precise path-of-divergence reason.

use crate::outcome::Outcome;
use crate::step_kind::StepKind;

/// How one step was judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepResult {
    /// The step's action ran and its expectation (if any) held.
    Pass,
    /// The observation contradicted the expectation.
    Fail {
        /// The path-of-divergence reason.
        reason: String,
    },
    /// The step could not be evaluated — a driver/transport error, a lowering
    /// failure, or an unresolved connection.
    Skipped {
        /// Why the step was skipped.
        reason: String,
    },
    /// An `outcome: unspecified` expectation: the observation is recorded, never
    /// judged.
    Unspecified {
        /// The outcome the driver actually reported.
        observed: Outcome,
    },
}

impl StepResult {
    /// Whether this result counts as a pass.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass)
    }

    /// Whether this result is a hard failure.
    #[must_use]
    pub fn is_fail(&self) -> bool {
        matches!(self, Self::Fail { .. })
    }
}

/// The record of one executed leaf step.
#[derive(Debug, Clone)]
pub struct StepTrace {
    /// Zero-based position within the flattened step program.
    pub index: usize,
    /// The step's action key.
    pub action: String,
    /// The step's typed action discriminant.
    pub kind: StepKind,
    /// The spec outcome the driver reported, when the action produced one.
    pub observed: Option<Outcome>,
    /// How the step was judged.
    pub result: StepResult,
}

impl StepTrace {
    /// Build a trace for `index`/`step_kind` with `observed` and `result`.
    #[must_use]
    pub fn new(index: usize, kind: StepKind, observed: Option<Outcome>, result: StepResult) -> Self {
        Self { index, action: kind.key().to_owned(), kind, observed, result }
    }
}
