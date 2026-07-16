//! The `expect` assertion block, shared by static-case loads and scenario steps.
//!
//! An `expect` names an [`Outcome`] and, for a success, optionally the returned
//! value (a [`Matcher`], or an `expect_one_of` disjunction) and the completion.
//! Non-`ok` non-`unspecified` outcomes name the violated rule in `violates`.
//! Members FORMAT.md leaves open per chapter (`frontier`, `holders`, `status`,
//! `result`, ...) are preserved raw in [`Expect::extra`] for the executor.

use serde_json::Value;

use crate::anchor::SpecAnchor;
use crate::error::{LoadError, Loc};
use crate::matcher::Matcher;
use crate::outcome::{Completion, Outcome};

/// A parsed `expect` / step-`expect` block.
#[derive(Debug, Clone)]
pub struct Expect {
    /// The asserted outcome. Absent on bare value assertions (implicit `ok`).
    pub outcome: Option<Outcome>,
    /// Violated-rule anchors; required for a non-`ok` non-`unspecified` outcome.
    pub violates: Vec<SpecAnchor>,
    /// Prose explaining an `unspecified` gap, when carried on the block itself.
    pub detail: Option<String>,
    /// The asserted return value.
    pub value: Option<Matcher>,
    /// A disjunction of spec-allowed results (`expect_one_of`).
    pub one_of: Option<Vec<Matcher>>,
    /// The asserted success completion.
    pub completion: Option<Completion>,
    /// Members outside the common vocabulary, verbatim, for chapter-specific use.
    pub extra: serde_json::Map<String, Value>,
}

impl Expect {
    /// Parse an `expect` object at `loc`.
    pub fn parse(value: &Value, loc: &Loc<'_>) -> Result<Self, LoadError> {
        let Value::Object(map) = value else {
            return Err(loc.error("expect must be an object"));
        };
        let mut expect = Self {
            outcome: None,
            violates: Vec::new(),
            detail: None,
            value: None,
            one_of: None,
            completion: None,
            extra: serde_json::Map::new(),
        };
        for (key, val) in map {
            match key.as_str() {
                "outcome" => expect.outcome = Some(parse_outcome(val, &loc.member("outcome"))?),
                "violates" => expect.violates = parse_violates(val, &loc.member("violates"))?,
                "detail" => expect.detail = Some(parse_string(val, &loc.member("detail"))?),
                "value" => expect.value = Some(Matcher::parse(val)),
                "expect_one_of" => expect.one_of = Some(parse_one_of(val, &loc.member("expect_one_of"))?),
                "completion" => expect.completion = Some(parse_completion(val, &loc.member("completion"))?),
                _ => {
                    expect.extra.insert(key.clone(), val.clone());
                }
            }
        }
        Ok(expect)
    }

    /// Check FORMAT.md's outcome/`violates` coupling. Returns a description of
    /// the first violation, or `None` when the block conforms.
    #[must_use]
    pub fn conformance_error(&self) -> Option<String> {
        match self.outcome {
            Some(o) if o.requires_violates() && self.violates.is_empty() => {
                Some(format!("outcome `{o}` requires a non-empty `violates`"))
            }
            Some(Outcome::Unspecified) if !self.violates.is_empty() => {
                Some("outcome `unspecified` must not carry `violates`".to_owned())
            }
            _ => None,
        }
    }
}

fn parse_outcome(value: &Value, loc: &Loc<'_>) -> Result<Outcome, LoadError> {
    let token = value.as_str().ok_or_else(|| loc.error("outcome must be a bare token string"))?;
    Outcome::parse(token).ok_or_else(|| loc.error(format!("`{token}` is not an outcome token")))
}

fn parse_completion(value: &Value, loc: &Loc<'_>) -> Result<Completion, LoadError> {
    let token = value.as_str().ok_or_else(|| loc.error("completion must be a bare token string"))?;
    Completion::parse(token).ok_or_else(|| loc.error(format!("`{token}` is not `committed` or `unchanged`")))
}

fn parse_string(value: &Value, loc: &Loc<'_>) -> Result<String, LoadError> {
    value.as_str().map(ToOwned::to_owned).ok_or_else(|| loc.error("expected a string"))
}

fn parse_violates(value: &Value, loc: &Loc<'_>) -> Result<Vec<SpecAnchor>, LoadError> {
    let items = value.as_array().ok_or_else(|| loc.error("violates must be an array of spec anchors"))?;
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            item.as_str()
                .map(SpecAnchor::new)
                .ok_or_else(|| loc.index(i).error("spec anchor must be a string"))
        })
        .collect()
}

fn parse_one_of(value: &Value, loc: &Loc<'_>) -> Result<Vec<Matcher>, LoadError> {
    let items = value.as_array().ok_or_else(|| loc.error("expect_one_of must be an array"))?;
    Ok(items.iter().map(Matcher::parse).collect())
}
