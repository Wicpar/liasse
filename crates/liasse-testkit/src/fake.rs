//! A scripted [`Driver`] double for exercising the engine without a runtime.
//!
//! Following the `liasse-host::sim` precedent (test doubles shipped with the
//! crate they exercise), [`FakeDriver`] answers value-producing actions
//! (`call`, `watch`, `read_view`, `op`) from a scripted queue of responses and
//! logs every request, so a test can drive the engine end to end and assert on
//! matcher semantics, binding flow, `operation_id` plumbing, `advance_time`
//! bookkeeping, `expect_one_of` branch judging, unspecified recording, and the
//! driver-error skip path. Lifecycle actions (`connect`, `disconnect`,
//! `advance_time`, `restart`, `unwatch`) succeed without consuming the queue.

use std::collections::VecDeque;
use std::fmt;

use serde_json::Value;

use crate::contract::{CallRequest, ConnectRequest, Driver, Observation, WatchRequest};
use crate::id::{ConnectionId, WatchId};
use crate::clock::Iso8601Duration;
use crate::request::OpRequest;

/// A scripted transport failure, used to exercise the engine's skip path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeError(pub String);

impl fmt::Display for FakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FakeError {}

/// One recorded request, for post-run assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeEvent {
    /// The action key (`call`, `watch`, `advance_time`, ...).
    pub action: String,
    /// The request target, rendered to a string (surface address, duration, id).
    pub target: String,
    /// The `operation_id`, when the request carried one.
    pub operation_id: Option<String>,
    /// The resolved connection, when the request ran on one.
    pub on: Option<String>,
}

/// A driver that replays scripted responses and records what it was asked.
#[derive(Debug, Default)]
pub struct FakeDriver {
    responses: VecDeque<Result<Observation, FakeError>>,
    log: Vec<FakeEvent>,
}

impl FakeDriver {
    /// An empty driver: value-producing actions default to a bare `ok`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue one response for the next value-producing action.
    #[must_use]
    pub fn respond(mut self, observation: Observation) -> Self {
        self.responses.push_back(Ok(observation));
        self
    }

    /// Queue a transport failure for the next value-producing action.
    #[must_use]
    pub fn fail(mut self, message: impl Into<String>) -> Self {
        self.responses.push_back(Err(FakeError(message.into())));
        self
    }

    /// The recorded request log, in dispatch order.
    #[must_use]
    pub fn log(&self) -> &[FakeEvent] {
        &self.log
    }

    /// Every recorded event whose action key is `action`.
    pub fn events(&self, action: &str) -> impl Iterator<Item = &FakeEvent> {
        self.log.iter().filter(move |e| e.action == action)
    }

    fn next(&mut self) -> Result<Observation, FakeError> {
        self.responses.pop_front().unwrap_or_else(|| Ok(Observation::ok(None)))
    }

    fn record(&mut self, action: &str, target: String, operation_id: Option<String>, on: Option<String>) {
        self.log.push(FakeEvent { action: action.to_owned(), target, operation_id, on });
    }
}

impl Driver for FakeDriver {
    type Error = FakeError;

    fn connect(&mut self, request: ConnectRequest) -> Result<Observation, Self::Error> {
        self.record("connect", request.connection.to_string(), None, Some(request.connection.to_string()));
        Ok(Observation::ok(None))
    }

    fn disconnect(&mut self, connection: &ConnectionId) -> Result<Observation, Self::Error> {
        self.record("disconnect", connection.to_string(), None, Some(connection.to_string()));
        Ok(Observation::ok(None))
    }

    fn call(&mut self, request: CallRequest) -> Result<Observation, Self::Error> {
        self.record("call", request.target.clone(), request.operation_id.clone(), request.on.map(|c| c.to_string()));
        self.next()
    }

    fn watch(&mut self, request: WatchRequest) -> Result<Observation, Self::Error> {
        self.record("watch", request.target.clone(), None, request.on.map(|c| c.to_string()));
        self.next()
    }

    fn unwatch(&mut self, id: &WatchId) -> Result<Observation, Self::Error> {
        self.record("unwatch", id.to_string(), None, None);
        Ok(Observation::ok(None))
    }

    fn read_view(&mut self, id: &WatchId) -> Result<Observation, Self::Error> {
        self.record("expect_view", id.to_string(), None, None);
        self.next()
    }

    fn advance_time(&mut self, duration: &Iso8601Duration) -> Result<Observation, Self::Error> {
        self.record("advance_time", format!("{duration:?}"), None, None);
        Ok(Observation::ok(None))
    }

    fn restart(&mut self) -> Result<Observation, Self::Error> {
        self.record("restart", String::new(), None, None);
        Ok(Observation::ok(None))
    }

    fn op(&mut self, request: &OpRequest) -> Result<Observation, Self::Error> {
        let target = match &request.target {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        self.record(request.action_key(), target, None, request.on.as_ref().map(ToString::to_string));
        self.next()
    }
}
