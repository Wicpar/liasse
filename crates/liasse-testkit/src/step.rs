//! The scenario step model.
//!
//! Every step is a JSON object whose leading member names the action; the rest
//! are modifiers. Rather than model the idiosyncratic payload of all ~50 step
//! keys, a [`Step`] captures the typed envelope every executor needs — the
//! [`StepKind`], the universal `on`/`expect` modifiers, and any nested step
//! groups — and preserves the action-specific payload and remaining modifiers
//! verbatim as JSON for the executor to interpret. `concurrently` branches and
//! `in_sandbox` bodies are parsed recursively so a chapter-local step nested
//! inside them is validated too.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::error::{LoadError, Loc};
use crate::expect::Expect;
use crate::id::ConnectionId;
use crate::step_kind::{StepKind, StepScope};

/// Step groups nested inside a step (`concurrently` / `in_sandbox`).
#[derive(Debug, Clone)]
pub enum Nested {
    /// No nested steps.
    None,
    /// A single ordered sub-program (an `in_sandbox` body).
    Serial(Vec<Step>),
    /// Several concurrently-interleaved branches (`concurrently`).
    Branches(Vec<Vec<Step>>),
}

impl Nested {
    /// The nested serial sub-program, or an empty slice.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        match self {
            Self::Serial(steps) => steps,
            _ => &[],
        }
    }

    /// The concurrent branches, or an empty slice.
    #[must_use]
    pub fn branches(&self) -> &[Vec<Step>] {
        match self {
            Self::Branches(branches) => branches,
            _ => &[],
        }
    }
}

/// One scenario step.
#[derive(Debug, Clone)]
pub struct Step {
    /// The typed action discriminant.
    pub kind: StepKind,
    /// The value bound to the action key (the call target, artifact spec, ...).
    pub target: Value,
    /// The `on` connection modifier, if present.
    pub on: Option<ConnectionId>,
    /// The step-level `expect` assertion, if present.
    pub expect: Option<Expect>,
    /// Remaining modifiers (`args`, `id`, `operation_id`, ...), verbatim.
    pub members: serde_json::Map<String, Value>,
    /// Parsed nested step groups.
    pub nested: Nested,
}

impl Step {
    /// The action key text.
    #[must_use]
    pub fn action_key(&self) -> &str {
        self.kind.key()
    }

    /// A remaining modifier by name.
    #[must_use]
    pub fn member(&self, name: &str) -> Option<&Value> {
        self.members.get(name)
    }

    /// Parse one step object against the chapter's `allowed` local key set.
    pub fn parse(value: &Value, loc: &Loc<'_>, allowed: &BTreeSet<String>) -> Result<Self, LoadError> {
        let Value::Object(map) = value else {
            return Err(loc.error("a step must be an object"));
        };
        let action = map
            .keys()
            .find(|k| k.as_str() != "on" && k.as_str() != "expect")
            .ok_or_else(|| loc.error("a step object names no action key"))?;
        let kind = StepKind::from_key(action);
        if kind.scope() == StepScope::Chapter && !allowed.contains(action) {
            return Err(loc.error(format!(
                "unknown step key `{action}`: chapter-local step keys must be documented in the chapter's NOTES.md"
            )));
        }

        let target = map.get(action).cloned().unwrap_or(Value::Null);
        let on = match map.get("on") {
            Some(v) => Some(ConnectionId::new(
                v.as_str().ok_or_else(|| loc.member("on").error("`on` must be a connection id string"))?,
            )),
            None => None,
        };
        let expect = match map.get("expect") {
            Some(v) => Some(Expect::parse(v, &loc.member("expect"))?),
            None => None,
        };
        let members = map
            .iter()
            .filter(|(k, _)| k.as_str() != action && k.as_str() != "on" && k.as_str() != "expect")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let nested = Self::parse_nested(&kind, &target, map, loc, allowed)?;

        Ok(Self { kind, target, on, expect, members, nested })
    }

    fn parse_nested(
        kind: &StepKind,
        target: &Value,
        map: &serde_json::Map<String, Value>,
        loc: &Loc<'_>,
        allowed: &BTreeSet<String>,
    ) -> Result<Nested, LoadError> {
        if matches!(kind, StepKind::Concurrently) {
            let branches = target
                .as_array()
                .ok_or_else(|| loc.member("concurrently").error("`concurrently` must be an array of branches"))?;
            let mut parsed = Vec::with_capacity(branches.len());
            for (i, branch) in branches.iter().enumerate() {
                let bloc = loc.member("concurrently").index(i);
                parsed.push(Self::parse_program(branch, &bloc, allowed)?);
            }
            return Ok(Nested::Branches(parsed));
        }
        if let Some(steps) = map.get("steps") {
            return Ok(Nested::Serial(Self::parse_program(steps, &loc.member("steps"), allowed)?));
        }
        Ok(Nested::None)
    }

    /// Parse an array of step objects.
    pub fn parse_program(value: &Value, loc: &Loc<'_>, allowed: &BTreeSet<String>) -> Result<Vec<Step>, LoadError> {
        let items = value.as_array().ok_or_else(|| loc.error("expected an array of steps"))?;
        items.iter().enumerate().map(|(i, item)| Step::parse(item, &loc.index(i), allowed)).collect()
    }
}
