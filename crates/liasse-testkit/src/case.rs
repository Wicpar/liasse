//! The typed case model and its parser.
//!
//! A case file is Hjson (parsed to a [`serde_json::Value`] via `deser-hjson`)
//! matching the FORMAT.md "Case shape". Embedded package and host definitions
//! are the language's own concern and are kept as raw JSON; everything the
//! harness reasons about — identity, suite, spec anchors, outcome, steps — is
//! parsed into types.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::anchor::SpecAnchor;
use crate::error::{LoadError, Loc};
use crate::expect::Expect;
use crate::step::Step;

/// Whether a case is built-and-inspected (`static`) or driven by steps
/// (`scenario`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suite {
    /// The package is loaded and `expect` states the load outcome.
    Static,
    /// The package loads, then `steps` run in order.
    Scenario,
}

impl Suite {
    fn parse(token: &str) -> Option<Self> {
        match token {
            "static" => Some(Self::Static),
            "scenario" => Some(Self::Scenario),
            _ => None,
        }
    }
}

/// The package(s) a case defines. Single-package cases use `package`;
/// module/migration cases use `packages` with a `root` label. Definitions are
/// kept verbatim — the language parser owns their meaning.
#[derive(Debug, Clone)]
pub enum PackageSet {
    /// One inline `package` definition.
    Single(Value),
    /// A `packages` map keyed by label, with the `root` label if declared.
    Multi {
        /// Label → raw package definition.
        packages: serde_json::Map<String, Value>,
        /// The root package label, if the case names one.
        root: Option<String>,
    },
}

/// The suite-specific body of a case.
#[derive(Debug, Clone)]
pub enum CaseBody {
    /// A `static` case's load-outcome assertion.
    Static(Expect),
    /// A `scenario` case's ordered step program.
    Scenario(Vec<Step>),
}

/// A fully parsed conformance case.
#[derive(Debug, Clone)]
pub struct Case {
    /// The `format` generation (always `1` in this corpus).
    pub format: u64,
    /// The case name (matches the filename stem).
    pub name: String,
    /// `static` or `scenario`.
    pub suite: Suite,
    /// SPEC.md anchors justifying every expectation.
    pub spec: Vec<SpecAnchor>,
    /// Optional classification tags.
    pub tags: Vec<String>,
    /// Optional prose; required for an `unspecified` outcome.
    pub note: Option<String>,
    /// The package(s) under test.
    pub packages: PackageSet,
    /// Optional simulated host components, verbatim.
    pub hosts: Option<Value>,
    /// The suite-specific body.
    pub body: CaseBody,
    /// Top-level members outside the FORMAT.md vocabulary (`resources`, ...).
    pub extra: serde_json::Map<String, Value>,
}

impl Case {
    /// Parse a case from raw Hjson text, tagging errors with `path`. The corpus
    /// dialect is normalized (see the `relax` pass) before decoding. `allowed`
    /// is the set of chapter-local step keys the chapter's `NOTES.md` documents.
    pub fn from_hjson(text: &str, path: &Path, allowed: &BTreeSet<String>) -> Result<Self, LoadError> {
        let normalized = crate::relax::normalize(text);
        let value: Value = deser_hjson::from_str(&normalized)
            .map_err(|err| LoadError::Syntax { path: path.to_path_buf(), message: err.to_string() })?;
        Self::parse(&value, &Loc::root(path), allowed)
    }

    /// Parse a case from its already-decoded Hjson value. `allowed` is the set
    /// of chapter-local step keys documented by the chapter's `NOTES.md`.
    pub fn parse(value: &Value, loc: &Loc<'_>, allowed: &BTreeSet<String>) -> Result<Self, LoadError> {
        let Value::Object(map) = value else {
            return Err(loc.error("a case file must be a JSON object"));
        };
        let format = require_u64(map, "format", loc)?;
        if format != 1 {
            return Err(loc.member("format").error(format!("unsupported case format `{format}`; expected 1")));
        }
        let name = require_str(map, "name", loc)?;
        let suite_token = require_str(map, "suite", loc)?;
        let suite = Suite::parse(&suite_token)
            .ok_or_else(|| loc.member("suite").error(format!("`{suite_token}` is not `static` or `scenario`")))?;
        let spec = parse_spec(map, loc)?;
        let tags = parse_tags(map, loc)?;
        let note = opt_str(map, "note", loc)?;
        let packages = parse_packages(map, loc)?;
        let hosts = map.get("hosts").cloned();
        let body = parse_body(map, suite, loc, allowed)?;
        let extra = collect_extra(map);

        Ok(Self { format, name, suite, spec, tags, note, packages, hosts, body, extra })
    }

    /// FORMAT.md conformance beyond well-formedness: the outcome/`violates`
    /// coupling and the `unspecified` explanation requirement. Returns the
    /// first violation description, or `None` when the case conforms.
    #[must_use]
    pub fn conformance_error(&self) -> Option<String> {
        match &self.body {
            CaseBody::Static(expect) => self.check_expect(expect),
            CaseBody::Scenario(steps) => self.check_steps(steps),
        }
    }

    fn check_expect(&self, expect: &Expect) -> Option<String> {
        if let Some(err) = expect.conformance_error() {
            return Some(err);
        }
        if expect.outcome == Some(crate::outcome::Outcome::Unspecified) && self.note.is_none() && expect.detail.is_none() {
            return Some("an `unspecified` outcome must carry a `note` or `detail` explaining the gap".to_owned());
        }
        None
    }

    fn check_steps(&self, steps: &[Step]) -> Option<String> {
        for step in steps {
            if let Some(expect) = &step.expect {
                // The `unspecified` explanation may live on the case note, so a
                // step block only owns the outcome/`violates` coupling here.
                if let Some(err) = expect.conformance_error() {
                    return Some(format!("step `{}`: {err}", step.action_key()));
                }
            }
            if let Some(err) = self.check_steps(step.nested.steps()) {
                return Some(err);
            }
            for branch in step.nested.branches() {
                if let Some(err) = self.check_steps(branch) {
                    return Some(err);
                }
            }
        }
        None
    }
}

fn parse_body(
    map: &serde_json::Map<String, Value>,
    suite: Suite,
    loc: &Loc<'_>,
    allowed: &BTreeSet<String>,
) -> Result<CaseBody, LoadError> {
    match suite {
        Suite::Static => {
            let value = map.get("expect").ok_or_else(|| loc.error("a `static` case must carry an `expect` block"))?;
            Ok(CaseBody::Static(Expect::parse(value, &loc.member("expect"))?))
        }
        Suite::Scenario => {
            let value = map.get("steps").ok_or_else(|| loc.error("a `scenario` case must carry a `steps` array"))?;
            Ok(CaseBody::Scenario(Step::parse_program(value, &loc.member("steps"), allowed)?))
        }
    }
}

fn parse_packages(map: &serde_json::Map<String, Value>, loc: &Loc<'_>) -> Result<PackageSet, LoadError> {
    if let Some(package) = map.get("package") {
        return Ok(PackageSet::Single(package.clone()));
    }
    if let Some(packages) = map.get("packages") {
        let obj = packages
            .as_object()
            .ok_or_else(|| loc.member("packages").error("`packages` must be a map of label to definition"))?;
        let root = opt_str(map, "root", loc)?;
        return Ok(PackageSet::Multi { packages: obj.clone(), root });
    }
    Err(loc.error("a case must declare `package` or `packages`"))
}

fn parse_spec(map: &serde_json::Map<String, Value>, loc: &Loc<'_>) -> Result<Vec<SpecAnchor>, LoadError> {
    let value = map.get("spec").ok_or_else(|| loc.error("a case must declare a `spec` array"))?;
    let items = value.as_array().ok_or_else(|| loc.member("spec").error("`spec` must be an array of anchors"))?;
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            item.as_str()
                .map(SpecAnchor::new)
                .ok_or_else(|| loc.member("spec").index(i).error("spec anchor must be a string"))
        })
        .collect()
}

fn parse_tags(map: &serde_json::Map<String, Value>, loc: &Loc<'_>) -> Result<Vec<String>, LoadError> {
    match map.get("tags") {
        None => Ok(Vec::new()),
        Some(value) => {
            let items = value.as_array().ok_or_else(|| loc.member("tags").error("`tags` must be an array"))?;
            items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    item.as_str().map(ToOwned::to_owned).ok_or_else(|| loc.member("tags").index(i).error("tag must be a string"))
                })
                .collect()
        }
    }
}

fn collect_extra(map: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    const KNOWN: [&str; 11] =
        ["format", "name", "suite", "spec", "tags", "note", "package", "packages", "root", "hosts", "expect"];
    map.iter()
        .filter(|(k, _)| !KNOWN.contains(&k.as_str()) && k.as_str() != "steps")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn require_str(map: &serde_json::Map<String, Value>, key: &str, loc: &Loc<'_>) -> Result<String, LoadError> {
    opt_str(map, key, loc)?.ok_or_else(|| loc.member(key).error("required string member is missing"))
}

fn opt_str(map: &serde_json::Map<String, Value>, key: &str, loc: &Loc<'_>) -> Result<Option<String>, LoadError> {
    match map.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(|s| Some(s.to_owned()))
            .ok_or_else(|| loc.member(key).error("expected a string")),
    }
}

fn require_u64(map: &serde_json::Map<String, Value>, key: &str, loc: &Loc<'_>) -> Result<u64, LoadError> {
    map.get(key)
        .ok_or_else(|| loc.member(key).error("required member is missing"))?
        .as_u64()
        .ok_or_else(|| loc.member(key).error("expected an unsigned integer"))
}
