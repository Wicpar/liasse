//! The scenario executor driven against the scripted [`FakeDriver`]. Every
//! expected result is deducible from the hand-written scenario and the scripted
//! responses, not from running the engine — the scripts stand in for a runtime.

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::fake::FakeDriver;
use liasse_testkit::{
    run_case, Area, Case, CaseBody, CaseVerdict, Completion, Engine, Observation, Outcome, Report, StepResult,
    StepTrace, SuiteKind,
};
use serde_json::{json, Value};

fn scenario(text: &str) -> Result<Case, String> {
    Case::from_hjson(text, Path::new("<case>"), &BTreeSet::new()).map_err(|e| e.to_string())
}

fn steps_of(case: &Case) -> Result<&[liasse_testkit::Step], String> {
    match &case.body {
        CaseBody::Scenario(steps) => Ok(steps),
        CaseBody::Static(_) => Err("expected a scenario body".into()),
    }
}

fn ok(value: Value) -> Observation {
    Observation::ok(Some(value))
}

fn ok_completed(value: Value, completion: Completion) -> Observation {
    Observation { completion: Some(completion), ..Observation::ok(Some(value)) }
}

const UUID: &str = "2f1c8b4a-1111-4222-8333-444455556666";

const FLOW: &str = r##"{
  format: 1
  name: engine-bind-ref-flow
  suite: scenario
  spec: ["#clients"]
  package: { $liasse: 1, $app: "t.x@1.0.0", $model: {} }
  steps: [
    { connect: "c1" }
    { call: "public.tasks.add", args: { title: "x" }, on: "c1",
      expect: { outcome: ok, value: { id: "$bind:t1", title: "x" } } }
    { call: "public.tasks.get", args: { id: "$ref:t1" }, on: "c1",
      expect: { outcome: ok, value: { id: "$ref:t1", title: "x" } } }
    { advance_time: "P31D" }
    { call: "public.tasks.add", args: { title: "y" }, operation_id: "op-7", on: "c1",
      expect: { outcome: ok, completion: committed, value: { id: "$any:uuid", title: "y" } } }
    { call: "public.tasks.add", args: { title: "y" }, operation_id: "op-7", on: "c1",
      expect: { outcome: ok, completion: unchanged, value: { id: "$any:uuid", title: "y" } } }
    { call: "public.tasks.pick", on: "c1",
      expect: { outcome: ok, expect_one_of: [ { id: "a" }, { id: "b" } ] } }
  ]
}"##;

#[test]
fn bind_ref_operation_id_clock_and_one_of_all_pass() -> Result<(), String> {
    let case = scenario(FLOW)?;
    let steps = steps_of(&case)?;
    let mut driver = FakeDriver::new()
        .respond(ok(json!({ "id": "abc", "title": "x" }))) // add x → binds t1 = "abc"
        .respond(ok(json!({ "id": "abc", "title": "x" }))) // get → $ref:t1 must equal "abc"
        .respond(ok_completed(json!({ "id": UUID, "title": "y" }), Completion::Committed))
        .respond(ok_completed(json!({ "id": UUID, "title": "y" }), Completion::Unchanged))
        .respond(ok(json!({ "id": "b" }))); // pick → matches the second alternative

    let mut engine = Engine::new(&mut driver);
    engine.run_program(steps);

    let failures: Vec<&StepTrace> = engine.traces().iter().filter(|t| !t.result.is_pass()).collect();
    assert!(failures.is_empty(), "every step should pass, got {failures:?}");
    assert_eq!(engine.traces().len(), 7, "one trace per leaf step");

    // The $bind capture is visible after the run and drives the $ref match.
    assert_eq!(engine.bindings().get("t1"), Some(&json!("abc")));
    // The clock advanced exactly once, by P31D: 2026-01-01 → 2026-02-01.
    assert_eq!(engine.clock().advance_count(), 1);
    assert_eq!(engine.clock().now().to_string(), "2026-02-01T00:00:00.000000Z");

    // The operation_id reached the driver on both submissions of the retry.
    let retries: Vec<_> = driver.events("call").filter(|e| e.operation_id.as_deref() == Some("op-7")).collect();
    assert_eq!(retries.len(), 2, "both op-7 calls carried the id to the driver");
    Ok(())
}

#[test]
fn implicit_connection_opens_for_a_single_client_case() -> Result<(), String> {
    // No `connect`, no `on`: the engine opens the implicit connection.
    let case = scenario(
        r##"{
          format: 1
          name: implicit-connect
          suite: scenario
          spec: ["#clients"]
          package: { $liasse: 1, $app: "t.x@1.0.0", $model: {} }
          steps: [
            { call: "public.tasks.add", args: { title: "x" },
              expect: { outcome: ok, value: { id: "$any", title: "x" } } }
          ]
        }"##,
    )?;
    let steps = steps_of(&case)?;
    let mut driver = FakeDriver::new().respond(ok(json!({ "id": "1", "title": "x" })));
    let mut engine = Engine::new(&mut driver);
    engine.run_program(steps);

    assert!(engine.traces().iter().all(|t| t.result.is_pass()), "{:?}", engine.traces());
    assert_eq!(driver.events("connect").count(), 1, "an implicit connect was issued");
    Ok(())
}

fn run(name: &str, text: &str, driver: &mut FakeDriver) -> Result<liasse_testkit::CaseResult, String> {
    let case = scenario(text)?;
    let _ = name;
    Ok(run_case(driver, &Area::new("t-engine"), SuiteKind::Common, &case))
}

#[test]
fn per_case_verdicts_aggregate_by_area() -> Result<(), String> {
    let base = |name: &str, expect: &str| {
        format!(
            r##"{{
              format: 1
              name: {name}
              suite: scenario
              spec: ["#clients"]
              package: {{ $liasse: 1, $app: "t.x@1.0.0", $model: {{}} }}
              steps: [ {{ call: "public.tasks.get", expect: {expect} }} ]
            }}"##
        )
    };

    // A clean pass.
    let mut d = FakeDriver::new().respond(ok(json!({ "id": "1" })));
    let pass = run("pass", &base("pass-case", r#"{ outcome: ok, value: { id: "1" } }"#), &mut d)?;
    assert_eq!(pass.verdict, CaseVerdict::Pass, "steps: {:?}", pass.steps);

    // A value divergence fails the case, with the path in the trace.
    let mut d = FakeDriver::new().respond(ok(json!({ "id": "wrong" })));
    let fail = run("fail", &base("fail-case", r#"{ outcome: ok, value: { id: "1" } }"#), &mut d)?;
    assert_eq!(fail.verdict, CaseVerdict::Fail { failures: 1 });
    let reason = fail.steps.iter().find_map(|s| match &s.result {
        StepResult::Fail { reason } => Some(reason.clone()),
        _ => None,
    });
    assert!(reason.as_deref().is_some_and(|r| r.contains("$.id")), "divergence path in reason, got {reason:?}");

    // An unspecified outcome is recorded, never judged.
    let mut d = FakeDriver::new().respond(Observation::outcome(Outcome::Error));
    let unspec = run("unspec", &base("unspec-case", "{ outcome: unspecified }"), &mut d)?;
    assert_eq!(unspec.verdict, CaseVerdict::UnspecifiedObservations { count: 1 });
    assert_eq!(unspec.steps.first().and_then(|s| s.observed), Some(Outcome::Error), "observed outcome is recorded");

    // A transport failure skips the case.
    let mut d = FakeDriver::new().fail("connection reset");
    let skip = run("skip", &base("skip-case", r#"{ outcome: ok, value: { id: "1" } }"#), &mut d)?;
    assert!(matches!(skip.verdict, CaseVerdict::Skipped { .. }), "got {:?}", skip.verdict);

    let mut report = Report::new();
    for result in [pass, fail, unspec, skip] {
        report.record(result);
    }
    let summary = report.summarize();
    assert_eq!(summary.total.passed, 1);
    assert_eq!(summary.total.failed, 1);
    assert_eq!(summary.total.skipped, 1);
    assert_eq!(summary.total.unspecified, 1);
    assert_eq!(summary.total.common, 4);
    assert!(!summary.is_clean(), "one case failed");
    let area = summary.by_area.get("t-engine").ok_or("area tally missing")?;
    assert_eq!(area.total(), 4);
    Ok(())
}

#[test]
fn concurrently_judges_each_branch_with_expect_one_of() -> Result<(), String> {
    // Each branch adds its own row and reads the view back; the peer's row may
    // or may not be covered yet, so each read admits both serializations.
    let case = scenario(
        r##"{
          format: 1
          name: concurrent-branches
          suite: scenario
          spec: ["#clients"]
          package: { $liasse: 1, $app: "t.x@1.0.0", $model: {} }
          steps: [
            { connect: "c1" }
            { connect: "c2" }
            { watch: "public.tasks", on: "c1", id: "w1", expect_init: { value: [] } }
            { watch: "public.tasks", on: "c2", id: "w2", expect_init: { value: [] } }
            { concurrently: [
              [
                { call: "public.tasks.add", args: { title: "a" }, on: "c1",
                  expect: { outcome: ok, value: { id: "$any", title: "a" } } }
                { expect_view: { watch: "w1" },
                  expect_one_of: [
                    { value: [ { id: "$any", title: "a" } ] }
                    { value: [ { id: "$any", title: "a" }, { id: "$any", title: "b" } ] }
                  ] }
              ]
              [
                { call: "public.tasks.add", args: { title: "b" }, on: "c2",
                  expect: { outcome: ok, value: { id: "$any", title: "b" } } }
                { expect_view: { watch: "w2" },
                  expect_one_of: [
                    { value: [ { id: "$any", title: "b" } ] }
                    { value: [ { id: "$any", title: "a" }, { id: "$any", title: "b" } ] }
                  ] }
              ]
            ] }
          ]
        }"##,
    )?;
    let steps = steps_of(&case)?;
    // watch w1 → [], watch w2 → [], branch1 call → a, branch1 view → [a],
    // branch2 call → b, branch2 view → [a,b] (the second serialization).
    let mut driver = FakeDriver::new()
        .respond(ok(json!([])))
        .respond(ok(json!([])))
        .respond(ok(json!({ "id": "t-a", "title": "a" })))
        .respond(ok(json!([{ "id": "t-a", "title": "a" }])))
        .respond(ok(json!({ "id": "t-b", "title": "b" })))
        .respond(ok(json!([{ "id": "t-a", "title": "a" }, { "id": "t-b", "title": "b" }])));

    let mut engine = Engine::new(&mut driver);
    engine.run_program(steps);
    let failures: Vec<&StepTrace> = engine.traces().iter().filter(|t| !t.result.is_pass()).collect();
    assert!(failures.is_empty(), "every branch step should pass, got {failures:?}");
    // 2 connects + 2 watches + (1 call + 1 view) * 2 branches = 8 leaf steps.
    assert_eq!(engine.traces().len(), 8);
    Ok(())
}
