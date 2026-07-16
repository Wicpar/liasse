//! Extracting and judging the value assertions carried by view steps.
//!
//! `watch`, `expect_view`, and `expect_init` do not carry a standard `expect`
//! block; their asserted value lives in an `expect_init` sub-block, in the
//! `expect_view` target object, or in a sibling `expect_one_of` list. This
//! module normalizes those shapes into a single [`ViewAssertion`] the engine
//! judges against the observed value, so the two spellings of `expect_one_of`
//! (each alternative a bare value, or an object wrapping a `value` member) are
//! handled uniformly.

use serde_json::Value;

use crate::matcher::{Bindings, Matcher};
use crate::report::Verdict;
use crate::step::Step;

/// The value assertion a view step makes, if any.
#[derive(Debug, Clone)]
pub enum ViewAssertion {
    /// The step asserts nothing about the observed value.
    None,
    /// The observed value must match exactly this matcher.
    Value(Matcher),
    /// The observed value must match one of these spec-allowed alternatives.
    OneOf(Vec<Matcher>),
}

impl ViewAssertion {
    /// The assertion a `watch` step makes via its `expect_init` sub-block.
    #[must_use]
    pub fn for_watch(step: &Step) -> Self {
        step.member("expect_init").map(Self::from_block).unwrap_or(Self::None)
    }

    /// The assertion an `expect_view` step makes: a `value` inside the target
    /// object, or a sibling `expect_one_of` list.
    #[must_use]
    pub fn for_expect_view(step: &Step) -> Self {
        if let Some(one_of) = step.member("expect_one_of").and_then(Value::as_array) {
            return Self::OneOf(one_of.iter().map(alternative).collect());
        }
        match step.target.get("value") {
            Some(value) => Self::Value(Matcher::parse(value)),
            None => Self::None,
        }
    }

    /// Normalize an `expect_init` block: a `value` matcher or an `expect_one_of`
    /// disjunction.
    fn from_block(block: &Value) -> Self {
        if let Some(one_of) = block.get("expect_one_of").and_then(Value::as_array) {
            return Self::OneOf(one_of.iter().map(alternative).collect());
        }
        match block.get("value") {
            Some(value) => Self::Value(Matcher::parse(value)),
            None => Self::None,
        }
    }

    /// Whether this assertion asserts anything.
    #[must_use]
    pub fn is_some(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Judge an observed value against this assertion, threading `env`.
    #[must_use]
    pub fn judge(&self, observed: Option<&Value>, env: &mut Bindings) -> Verdict {
        let matchers: Vec<&Matcher> = match self {
            Self::None => return Verdict::Pass,
            Self::Value(m) => vec![m],
            Self::OneOf(alts) => alts.iter().collect(),
        };
        let Some(value) = observed else {
            return Verdict::Fail { reason: "expected a view value, none observed".to_owned() };
        };
        let mut last = None;
        for matcher in matchers {
            let mut trial = env.clone();
            match matcher.check(value, &mut trial) {
                Ok(()) => {
                    *env = trial;
                    return Verdict::Pass;
                }
                Err(err) => last = Some(err),
            }
        }
        Verdict::Fail {
            reason: match last {
                Some(err) => format!("view value mismatch: {err}"),
                None => "view value matched no alternative".to_owned(),
            },
        }
    }
}

/// Parse one `expect_one_of` alternative: an object wrapping a `value` member,
/// or the alternative value itself.
fn alternative(alt: &Value) -> Matcher {
    match alt.get("value") {
        Some(value) => Matcher::parse(value),
        None => Matcher::parse(alt),
    }
}
