//! Step-vocabulary gating and end-to-end parsing of the corpus Hjson dialect,
//! driven through the in-memory [`Case::from_hjson`] entry point.

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{Case, CaseBody, LoadError, Outcome, StepKind};

fn allowed(keys: &[&str]) -> BTreeSet<String> {
    keys.iter().map(|k| (*k).to_owned()).collect()
}

fn parse(text: &str, keys: &[&str]) -> Result<Case, LoadError> {
    Case::from_hjson(text, Path::new("<case>"), &allowed(keys))
}

const SCENARIO: &str = r##"{
  format: 1
  name: sample-scenario
  suite: scenario
  spec: ["#modules", "§13.3"]
  package: { $liasse: 1, $app: "t.x@1.0.0", $model: {} }
  steps: [
    { call: "public.tasks.add", args: { title: "x" }, expect: { outcome: ok, value: { id: "$bind:t1", title: "x" } } }
    { widget_poke: { foo: 1 }, expect: { outcome: ok } }
  ]
}"##;

#[test]
fn a_documented_chapter_step_is_accepted() -> Result<(), String> {
    let case = parse(SCENARIO, &["widget_poke"]).map_err(|e| e.to_string())?;
    let CaseBody::Scenario(steps) = &case.body else {
        return Err("expected a scenario body".into());
    };
    assert_eq!(steps.len(), 2);
    assert_eq!(steps.first().map(|s| s.kind.clone()), Some(StepKind::Call));
    assert_eq!(steps.get(1).map(|s| s.kind.clone()), Some(StepKind::Chapter("widget_poke".to_owned())));
    Ok(())
}

#[test]
fn an_undocumented_chapter_step_is_rejected_by_name() -> Result<(), String> {
    // `widget_poke` is not documented for this chapter's NOTES.md.
    let LoadError::Shape { field, message, .. } = parse(SCENARIO, &[]).err().ok_or("undocumented step must be rejected")? else {
        return Err("expected a shape error".into());
    };
    assert!(field.contains("steps[1]"), "error should point at the offending step, got `{field}`");
    assert!(message.contains("widget_poke"), "error should name the key, got `{message}`");
    assert!(message.contains("NOTES.md"), "error should explain the rule, got `{message}`");
    Ok(())
}

#[test]
fn a_global_builtin_step_needs_no_documentation() -> Result<(), String> {
    let only_builtins = r##"{
      format: 1
      name: builtin-only
      suite: scenario
      spec: ["#clients"]
      package: { $liasse: 1, $app: "t.x@1.0.0", $model: {} }
      steps: [
        { call: "public.tasks.add", args: { title: "x" }, expect: { outcome: ok } }
        { restart: {} }
      ]
    }"##;
    // Empty allowed set: `call` and `restart` are global and still parse.
    let case = parse(only_builtins, &[]).map_err(|e| e.to_string())?;
    let CaseBody::Scenario(steps) = &case.body else {
        return Err("expected a scenario body".into());
    };
    assert_eq!(steps.get(1).map(|s| s.kind.clone()), Some(StepKind::Restart));
    Ok(())
}

/// The relax pass must round-trip the corpus dialect: inline bare outcome
/// tokens, kebab-case names, and bare array tokens.
#[test]
fn relaxed_dialect_parses_bare_tokens_faithfully() -> Result<(), String> {
    let text = r##"{
      format: 1
      name: sample-static-case-with-dashes
      suite: static
      spec: ["#state-model", "§5.1"]
      tags: [normalize, defaults]
      package: { $liasse: 1, $app: "t.x@1.0.0", $model: {} }
      expect: { outcome: invalid, violates: ["#refs"], detail: "field references a missing collection" }
    }"##;
    let case = parse(text, &[]).map_err(|e| e.to_string())?;
    assert_eq!(case.name, "sample-static-case-with-dashes");
    assert_eq!(case.tags, vec!["normalize".to_owned(), "defaults".to_owned()]);
    let CaseBody::Static(expect) = &case.body else {
        return Err("expected a static body".into());
    };
    assert_eq!(expect.outcome, Some(Outcome::Invalid));
    assert_eq!(expect.violates.len(), 1);
    assert_eq!(expect.violates.first().map(|a| a.as_str().to_owned()), Some("#refs".to_owned()));
    Ok(())
}

/// The relax pass must quote JSON-invalid number spellings that `f64::parse`
/// would accept — leading zeros, a leading `+`, a bare fraction — while leaving
/// genuine JSON numbers unquoted. Otherwise a non-JSON bareword slips through as
/// a number. Probes ride in as top-level `extra` members, kept verbatim.
#[test]
fn relax_quotes_json_invalid_number_spellings() -> Result<(), String> {
    let text = r##"{
      format: 1
      name: json-invalid-number-spellings
      suite: static
      spec: ["§A.7"]
      package: { $liasse: 1, $app: "t.x@1.0.0", $model: {} }
      expect: { outcome: ok }
      leading_zero: 007
      leading_plus: +5
      bare_fraction: .5
      real_number: 1e5
    }"##;
    let case = parse(text, &[]).map_err(|e| e.to_string())?;
    let as_str = |key: &str| case.extra.get(key).and_then(|v| v.as_str()).map(str::to_owned);
    // JSON-forbidden spellings are quoted verbatim, so they survive as strings.
    assert_eq!(as_str("leading_zero"), Some("007".to_owned()));
    assert_eq!(as_str("leading_plus"), Some("+5".to_owned()));
    assert_eq!(as_str("bare_fraction"), Some(".5".to_owned()));
    // A genuine JSON number stays a number (the grammar is not over-tightened).
    assert!(
        case.extra.get("real_number").is_some_and(|v| v.is_number()),
        "1e5 is valid JSON and must stay a number, got {:?}",
        case.extra.get("real_number")
    );
    Ok(())
}

#[test]
fn a_non_ok_outcome_without_violates_is_a_conformance_error() -> Result<(), String> {
    let text = r##"{
      format: 1
      name: bad-static
      suite: static
      spec: ["#refs"]
      package: { $liasse: 1, $app: "t.x@1.0.0", $model: {} }
      expect: { outcome: rejected, detail: "missing violates" }
    }"##;
    let case = parse(text, &[]).map_err(|e| e.to_string())?;
    assert!(case.conformance_error().is_some(), "a rejected outcome must require violates");
    Ok(())
}
