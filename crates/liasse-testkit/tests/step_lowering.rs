//! Executor-side twin of the loader conformance test: every step of every
//! scenario case in the corpus lowers to a typed [`Request`] — no unknown-step
//! surprises reach a driver — and the core client verbs lower to their own
//! specific variants rather than the generic op bucket.

use liasse_testkit::{
    Bindings, Case, CaseBody, Corpus, HostKind, HostsConfig, Request, Step, StepKind, is_structural,
};

/// Recursively lower every leaf step, recording failures and tallying which
/// request variant each core verb produced.
fn lower_all(steps: &[Step], case: &str, failures: &mut Vec<String>, seen: &mut Seen) {
    let env = Bindings::new();
    for step in steps {
        if is_structural(&step.kind) {
            for branch in step.nested.branches() {
                lower_all(branch, case, failures, seen);
            }
            lower_all(step.nested.steps(), case, failures, seen);
            continue;
        }
        match Request::lower(step, &env) {
            Ok(request) => seen.tally(&step.kind, &request),
            Err(err) => failures.push(format!("{case}: {err}")),
        }
    }
}

#[derive(Default)]
struct Seen {
    connect: bool,
    call: bool,
    watch: bool,
    read_view: bool,
    advance_time: bool,
    restart: bool,
    disconnect: bool,
    unwatch: bool,
    op: bool,
    /// A core verb that lowered to the generic op bucket — a design error.
    misrouted: Vec<String>,
}

impl Seen {
    fn tally(&mut self, kind: &StepKind, request: &Request) {
        match (kind, request) {
            (StepKind::Connect, Request::Connect(_)) => self.connect = true,
            (StepKind::Call, Request::Call(_)) => self.call = true,
            (StepKind::Watch, Request::Watch(_)) => self.watch = true,
            (StepKind::ExpectView, Request::ReadView(_)) => self.read_view = true,
            (StepKind::AdvanceTime, Request::AdvanceTime(_)) => self.advance_time = true,
            (StepKind::Restart, Request::Restart) => self.restart = true,
            (StepKind::Disconnect, Request::Disconnect(_)) => self.disconnect = true,
            (StepKind::Unwatch, Request::Unwatch(_)) => self.unwatch = true,
            (_, Request::Op(_)) => self.op = true,
            // A core verb produced a non-matching request: a lowering bug.
            (kind, _) => self.misrouted.push(kind.key().to_owned()),
        }
    }
}

#[test]
fn every_scenario_step_lowers_to_a_typed_request() -> Result<(), String> {
    let corpus = Corpus::load().map_err(|e| e.to_string())?;
    let mut failures = Vec::new();
    let mut seen = Seen::default();
    let mut scenario_cases = 0usize;

    for loaded in &corpus.cases {
        let CaseBody::Scenario(steps) = &loaded.case.body else { continue };
        scenario_cases += 1;
        lower_all(steps, &loaded.path.display().to_string(), &mut failures, &mut seen);
    }

    assert!(failures.is_empty(), "{} step(s) failed to lower:\n{}", failures.len(), failures.join("\n"));
    assert!(scenario_cases >= 400, "expected the full scenario corpus, found {scenario_cases}");
    assert!(seen.misrouted.is_empty(), "core verbs misrouted to a wrong request: {:?}", seen.misrouted);

    // The corpus exercises each core verb; each must reach its own variant.
    for (present, name) in [
        (seen.connect, "connect"),
        (seen.call, "call"),
        (seen.watch, "watch"),
        (seen.read_view, "expect_view"),
        (seen.advance_time, "advance_time"),
        (seen.restart, "restart"),
        (seen.op, "op (registry/chapter)"),
    ] {
        assert!(present, "expected at least one `{name}` step lowered to its variant");
    }
    Ok(())
}

#[test]
fn host_blocks_parse_into_typed_components() -> Result<(), String> {
    let corpus = Corpus::load().map_err(|e| e.to_string())?;
    let mut with_hosts = 0usize;
    let mut typed_components = 0usize;
    for loaded in &corpus.cases {
        let config = HostsConfig::from_case(&loaded.case);
        if loaded.case.hosts.is_some() {
            with_hosts += 1;
        }
        typed_components += config.of_kind(HostKind::Namespace).count()
            + config.of_kind(HostKind::KeyProvider).count()
            + config.of_kind(HostKind::Connector).count();
    }
    assert!(with_hosts >= 100, "corpus should carry many host blocks, found {with_hosts}");
    assert!(typed_components >= 100, "host blocks should yield typed components, found {typed_components}");
    Ok(())
}

/// The lowering of a single known step is what FORMAT.md prescribes, checked
/// without the corpus so the mapping itself is pinned.
#[test]
fn advance_time_lowers_to_a_parsed_duration() -> Result<(), String> {
    let case = Case::from_hjson(
        r##"{
          format: 1
          name: t
          suite: scenario
          spec: ["#runtime"]
          package: { $liasse: 1, $app: "t.x@1.0.0", $model: {} }
          steps: [ { advance_time: "PT1H" } { advance_time: "not-a-duration" } ]
        }"##,
        std::path::Path::new("<case>"),
        &std::collections::BTreeSet::new(),
    )
    .map_err(|e| e.to_string())?;
    let CaseBody::Scenario(steps) = &case.body else { return Err("scenario expected".into()) };
    let env = Bindings::new();
    let first = steps.first().ok_or("missing step")?;
    assert!(matches!(Request::lower(first, &env), Ok(Request::AdvanceTime(_))));
    // A malformed duration is a precise lowering error, not a silent Op.
    let second = steps.get(1).ok_or("missing step")?;
    assert!(Request::lower(second, &env).is_err(), "a bad duration must not lower");
    Ok(())
}
