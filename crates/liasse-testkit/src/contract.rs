//! The harness-facing execution contract.
//!
//! A future runtime adapter implements [`Executor`] to drive a real runtime and
//! store; the loader and matcher never depend on it, so an executor can be
//! written against this trait alone. Requests carry typed handles with raw
//! `serde_json` payloads at the boundary (arguments, authenticator selections,
//! window specs are the language's concern, not the harness's). Every action
//! yields an [`Observation`]: the spec-level outcome plus any observed value,
//! which the caller matches against the case's expectations. A `Result::Err`
//! is reserved for a harness/transport failure — a *denied* or *rejected*
//! request is a successful observation of that outcome, not an error.

use serde_json::Value;

use crate::id::{ConnectionId, WatchId};
use crate::outcome::{Completion, Outcome};
use crate::step::Step;

/// A request to open a logical client connection.
#[derive(Debug, Clone)]
pub struct ConnectRequest {
    /// The connection handle to open.
    pub connection: ConnectionId,
    /// The `authenticate` payload, verbatim (§11 mechanism), if any.
    pub authenticate: Option<Value>,
}

/// A request to invoke an external mutation surface.
#[derive(Debug, Clone)]
pub struct CallRequest {
    /// The dotted surface address (`public.tasks.add`).
    pub target: String,
    /// The argument object, verbatim (with `$ref:` already resolved).
    pub args: Value,
    /// The connection to submit on; `None` uses the sole connection.
    pub on: Option<ConnectionId>,
    /// The §12.3 operation identifier, if attached.
    pub operation_id: Option<String>,
    /// A per-request authenticator selection, verbatim, if attached.
    pub auth: Option<Value>,
}

/// A request to open a live subscription over a surface view.
#[derive(Debug, Clone)]
pub struct WatchRequest {
    /// The dotted surface address.
    pub target: String,
    /// The subscription handle.
    pub id: WatchId,
    /// The connection to open on; `None` uses the sole connection.
    pub on: Option<ConnectionId>,
    /// The view parameters, verbatim.
    pub args: Value,
    /// The §12.2 bounded-window spec, verbatim, if any.
    pub window: Option<Value>,
}

/// The observed result of performing an action.
#[derive(Debug, Clone)]
pub struct Observation {
    /// The spec-level outcome the runtime reported.
    pub outcome: Outcome,
    /// The returned value, when the action produces one.
    pub value: Option<Value>,
    /// The success completion, when reported.
    pub completion: Option<Completion>,
    /// Any additional observed members (frontier, holders, status, ...).
    pub extra: serde_json::Map<String, Value>,
}

impl Observation {
    /// A bare `ok` observation carrying `value`.
    #[must_use]
    pub fn ok(value: Option<Value>) -> Self {
        Self { outcome: Outcome::Ok, value, completion: None, extra: serde_json::Map::new() }
    }

    /// A bare observation of `outcome` with no value.
    #[must_use]
    pub fn outcome(outcome: Outcome) -> Self {
        Self { outcome, value: None, completion: None, extra: serde_json::Map::new() }
    }
}

/// The runtime + store adapter a harness drives.
pub trait Executor {
    /// The adapter's transport/host error type (not a spec outcome).
    type Error: std::error::Error;

    /// Open a client connection.
    fn connect(&mut self, request: ConnectRequest) -> Result<Observation, Self::Error>;

    /// Close a client connection.
    fn disconnect(&mut self, connection: &ConnectionId) -> Result<Observation, Self::Error>;

    /// Invoke an external mutation surface.
    fn call(&mut self, request: CallRequest) -> Result<Observation, Self::Error>;

    /// Open a live subscription; the observation carries the `init` value.
    fn watch(&mut self, request: WatchRequest) -> Result<Observation, Self::Error>;

    /// Close a subscription.
    fn unwatch(&mut self, id: &WatchId) -> Result<Observation, Self::Error>;

    /// Read the current value of a subscription after prior commits settle.
    fn read_view(&mut self, id: &WatchId) -> Result<Observation, Self::Error>;

    /// Advance the virtual clock by an ISO-8601 duration.
    fn advance_time(&mut self, duration: &str) -> Result<Observation, Self::Error>;

    /// Stop and replay the runtime; durable state must survive.
    fn restart(&mut self) -> Result<Observation, Self::Error>;

    /// Perform any step outside the typed client set — every registry and
    /// chapter-local step (`host_load`, `module_*`, `export`/`import`,
    /// `blob_*`, `operator`, `tamper_artifact`, ...). The executor matches on
    /// [`Step::kind`] and reads [`Step::target`]/[`Step::members`].
    fn perform(&mut self, step: &Step) -> Result<Observation, Self::Error>;
}
