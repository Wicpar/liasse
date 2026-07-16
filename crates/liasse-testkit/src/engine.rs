//! The scenario executor core.
//!
//! [`Engine`] interprets a loaded `scenario` [`Case`] step by step against a
//! [`Driver`]: it lowers each step to a typed [`Request`], resolves the default
//! connection (FORMAT.md: `on` defaults to the sole connection, and a single-
//! client case may skip `connect` entirely), advances the [`VirtualClock`],
//! dispatches to the driver, and judges the observation — threading one
//! [`Bindings`] so `$bind:` captures reach later `$ref:` uses. `concurrently`
//! branches run in sequence (the driver owns serialization; per-branch
//! `expect_one_of` admits the several serializations the spec allows), and an
//! `outcome: unspecified` step is recorded without judgement. The result is a
//! [`CaseResult`] with one [`StepTrace`] per leaf step.

use crate::case::{Case, CaseBody};
use crate::clock::VirtualClock;
use crate::contract::{ConnectRequest, Driver, Observation, is_structural};
use crate::corpus::{Area, LoadedCase, SuiteKind};
use crate::id::ConnectionId;
use crate::matcher::Bindings;
use crate::outcome::Outcome;
use crate::report::{CaseResult, CaseVerdict, Verdict, check_expectation};
use crate::request::Request;
use crate::step::Step;
use crate::step_kind::StepKind;
use crate::trace::{StepResult, StepTrace};
use crate::view::ViewAssertion;

/// The handle a single-client case gets when it never calls `connect`.
const IMPLICIT_CONNECTION: &str = "$default";

/// Run a loaded scenario case against `driver`, producing its verdict and trace.
/// A `static` case has no steps to drive and is reported as skipped.
pub fn run_loaded<D: Driver>(driver: &mut D, loaded: &LoadedCase) -> CaseResult {
    run_case(driver, &loaded.area, loaded.suite_kind, &loaded.case)
}

/// Run a `case` against `driver`, tagging the result with `area`/`suite`.
pub fn run_case<D: Driver>(driver: &mut D, area: &Area, suite: SuiteKind, case: &Case) -> CaseResult {
    let name = case.name.clone();
    match &case.body {
        CaseBody::Static(_) => CaseResult {
            area: area.clone(),
            suite_kind: suite,
            name,
            verdict: CaseVerdict::Skipped { reason: "static case carries no scenario steps".to_owned() },
            steps: Vec::new(),
        },
        CaseBody::Scenario(steps) => {
            let mut engine = Engine::new(driver);
            engine.run_program(steps);
            let steps = engine.into_traces();
            CaseResult { area: area.clone(), suite_kind: suite, name, verdict: CaseVerdict::from_steps(&steps), steps }
        }
    }
}

/// The stateful interpreter for one case: a driver, the binding environment, the
/// virtual clock, the set of open connections, and the accumulating trace.
pub struct Engine<'d, D: Driver> {
    driver: &'d mut D,
    env: Bindings,
    clock: VirtualClock,
    connections: Vec<ConnectionId>,
    traces: Vec<StepTrace>,
    next_index: usize,
}

impl<'d, D: Driver> Engine<'d, D> {
    /// A fresh interpreter over `driver`, clock at the FORMAT.md epoch.
    pub fn new(driver: &'d mut D) -> Self {
        Self {
            driver,
            env: Bindings::new(),
            clock: VirtualClock::new(),
            connections: Vec::new(),
            traces: Vec::new(),
            next_index: 0,
        }
    }

    /// The accumulated step traces.
    #[must_use]
    pub fn traces(&self) -> &[StepTrace] {
        &self.traces
    }

    /// Consume the interpreter, yielding its traces.
    #[must_use]
    pub fn into_traces(self) -> Vec<StepTrace> {
        self.traces
    }

    /// The binding environment threaded through the run.
    #[must_use]
    pub fn bindings(&self) -> &Bindings {
        &self.env
    }

    /// The virtual clock after every applied `advance_time`.
    #[must_use]
    pub fn clock(&self) -> &VirtualClock {
        &self.clock
    }

    /// Run an ordered step program, recursing into nested groups in place.
    pub fn run_program(&mut self, steps: &[Step]) {
        for step in steps {
            self.run_step(step);
        }
    }

    fn run_step(&mut self, step: &Step) {
        if is_structural(&step.kind) {
            match step.kind {
                StepKind::Concurrently => {
                    for branch in step.nested.branches() {
                        self.run_program(branch);
                    }
                }
                StepKind::InSandbox => {
                    // §19.10: the driver isolates an instance for the group so a
                    // `restore`/`export` inside cannot perturb the outer one.
                    let name = step.target.as_str().unwrap_or_default();
                    let fresh = step.member("fresh").and_then(serde_json::Value::as_bool).unwrap_or(false);
                    if self.driver.enter_sandbox(name, fresh).is_ok() {
                        self.run_program(step.nested.steps());
                        let _ = self.driver.exit_sandbox();
                    }
                }
                _ => self.run_program(step.nested.steps()),
            }
            return;
        }

        let index = self.take_index();
        let request = match Request::lower(step, &self.env) {
            Ok(request) => request,
            Err(err) => return self.record(index, &step.kind, None, StepResult::Skipped { reason: err.to_string() }),
        };
        let request = match self.resolve_connection(request) {
            Ok(request) => request,
            Err(reason) => return self.record(index, &step.kind, None, StepResult::Skipped { reason }),
        };
        if let Request::AdvanceTime(duration) = &request {
            self.clock.advance(duration);
        }
        let observation = match self.driver.dispatch(&request) {
            Ok(observation) => observation,
            Err(err) => {
                return self.record(index, &step.kind, None, StepResult::Skipped { reason: format!("driver error: {err}") });
            }
        };
        if let Request::Connect(connect) = &request {
            self.register(connect.connection.clone());
        }
        let result = self.judge(step, &observation);
        self.record(index, &step.kind, Some(observation.outcome), result);
    }

    /// Fill a `Call`/`Watch` request's connection when `on` is omitted, auto-
    /// opening the implicit connection for a single-client case if none is open.
    fn resolve_connection(&mut self, request: Request) -> Result<Request, String> {
        match request {
            Request::Call(mut call) => {
                call.on = Some(self.effective_connection(call.on)?);
                Ok(Request::Call(call))
            }
            Request::Watch(mut watch) => {
                watch.on = Some(self.effective_connection(watch.on)?);
                Ok(Request::Watch(watch))
            }
            other => Ok(other),
        }
    }

    fn effective_connection(&mut self, on: Option<ConnectionId>) -> Result<ConnectionId, String> {
        if let Some(connection) = on {
            self.register(connection.clone());
            return Ok(connection);
        }
        match self.connections.as_slice() {
            [only] => Ok(only.clone()),
            [] => self.open_implicit(),
            _ => Err("ambiguous connection: several are open, so the step must name `on`".to_owned()),
        }
    }

    fn open_implicit(&mut self) -> Result<ConnectionId, String> {
        let connection = ConnectionId::new(IMPLICIT_CONNECTION);
        let request = ConnectRequest { connection: connection.clone(), authenticate: None };
        match self.driver.connect(request) {
            Ok(_) => {
                self.register(connection.clone());
                Ok(connection)
            }
            Err(err) => Err(format!("implicit connect failed: {err}")),
        }
    }

    fn judge(&mut self, step: &Step, observation: &Observation) -> StepResult {
        if let Some(expect) = &step.expect {
            if expect.outcome == Some(Outcome::Unspecified) {
                return StepResult::Unspecified { observed: observation.outcome };
            }
            let verdict = check_expectation(expect, observation, &mut self.env);
            if !verdict.is_pass() {
                return into_result(verdict);
            }
        }
        let assertion = match step.kind {
            StepKind::Watch => ViewAssertion::for_watch(step),
            StepKind::ExpectView => ViewAssertion::for_expect_view(step),
            _ => ViewAssertion::None,
        };
        if assertion.is_some() {
            return into_result(assertion.judge(observation.value.as_ref(), &mut self.env));
        }
        StepResult::Pass
    }

    fn register(&mut self, connection: ConnectionId) {
        if !self.connections.contains(&connection) {
            self.connections.push(connection);
        }
    }

    fn record(&mut self, index: usize, kind: &StepKind, observed: Option<Outcome>, result: StepResult) {
        self.traces.push(StepTrace::new(index, kind.clone(), observed, result));
    }

    fn take_index(&mut self) -> usize {
        let index = self.next_index;
        self.next_index += 1;
        index
    }
}

fn into_result(verdict: Verdict) -> StepResult {
    match verdict {
        Verdict::Pass => StepResult::Pass,
        Verdict::Fail { reason } => StepResult::Fail { reason },
        Verdict::Skipped { reason } => StepResult::Skipped { reason },
    }
}
