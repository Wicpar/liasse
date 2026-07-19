//! The harness-facing execution contract.
//!
//! A future runtime adapter implements [`Driver`] to drive a real runtime and
//! store; the loader, matcher, and engine never depend on a concrete driver, so
//! one can be written against this trait alone. Requests carry typed handles
//! with raw `serde_json` payloads at the boundary (arguments, authenticator
//! selections, window specs are the language's concern, not the harness's).
//! Every action yields an [`Observation`]: the spec-level outcome plus any
//! observed value, which the engine matches against a case's expectations. A
//! `Result::Err` is reserved for a harness/transport failure — a *denied* or
//! *rejected* request is a successful observation of that outcome, not an error.

use serde_json::Value;

use crate::clock::Iso8601Duration;
use crate::id::ConnectionId;
use crate::outcome::{Completion, Outcome};
use crate::request::{OpRequest, Request};
use crate::step_kind::StepKind;

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
    /// The named authentication context on a multiplexed connection (§11.8), if
    /// the step selects one.
    pub context: Option<String>,
    /// The scope-row key a scoped-role call is addressed under (§10.3/§10.5),
    /// verbatim — the containing row identity whose surface the call targets. The
    /// driver decodes it against the scope collection's key type.
    pub scope: Option<Value>,
    /// The covered-descendant key path a §10.5 call addresses, verbatim — the key
    /// path from the role-holding row down through `$field`/`$through` to the
    /// covered descendant the mutation receiver binds to.
    pub descendant: Option<Value>,
}

/// A request to open a live subscription over a surface view.
#[derive(Debug, Clone)]
pub struct WatchRequest {
    /// The dotted surface address.
    pub target: String,
    /// The subscription handle.
    pub id: crate::id::WatchId,
    /// The connection to open on; `None` uses the sole connection.
    pub on: Option<ConnectionId>,
    /// The view parameters, verbatim.
    pub args: Value,
    /// The §12.2 bounded-window spec, verbatim, if any.
    pub window: Option<Value>,
    /// A per-request authenticator selection, verbatim, if attached (§11.4): a
    /// subscription that authenticates inline rather than reusing a connection
    /// context.
    pub auth: Option<Value>,
    /// The named authentication context on a multiplexed connection (§11.8), if
    /// the step selects one.
    pub context: Option<String>,
    /// The scope-row key a scoped-role subscription is addressed under (§10.5),
    /// verbatim — the containing row identity whose surface `$view` is watched. The
    /// driver decodes it against the scope collection's key type.
    pub scope: Option<Value>,
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

    /// An additional observed member by name (`frontier`, `status`, ...).
    #[must_use]
    pub fn extra(&self, name: &str) -> Option<&Value> {
        self.extra.get(name)
    }
}

/// The runtime + store adapter the engine drives.
///
/// The core client verbs are typed methods; the long tail of registry and
/// chapter-local steps (`export`/`import`, `blob_*`, module lifecycle,
/// `host_load`, `operator`, artifact `tamper`/`build`/`load`, ...) arrives as a
/// typed [`OpRequest`] through [`Driver::op`]. The engine routes a lowered
/// [`Request`] to the right method via [`Driver::dispatch`].
pub trait Driver {
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
    fn unwatch(&mut self, id: &crate::id::WatchId) -> Result<Observation, Self::Error>;

    /// Read the current value of a subscription after prior commits settle.
    fn read_view(&mut self, id: &crate::id::WatchId) -> Result<Observation, Self::Error>;

    /// Advance the virtual clock by an ISO-8601 duration.
    fn advance_time(&mut self, duration: &Iso8601Duration) -> Result<Observation, Self::Error>;

    /// Stop and replay the runtime; durable state must survive.
    fn restart(&mut self) -> Result<Observation, Self::Error>;

    /// Perform any step outside the typed client set. The executor matches on
    /// [`OpRequest::kind`] and reads its target/members.
    fn op(&mut self, request: &OpRequest) -> Result<Observation, Self::Error>;

    /// Enter an `in_sandbox` group (§19.10): the driver isolates an instance so a
    /// `restore`/`export` inside the group cannot perturb the outer one. `fresh`
    /// requests an independent installation of the case package (its own genesis
    /// and incarnation) rather than an instance a later `restore` activates. The
    /// default is a no-op for a driver that does not model sandbox isolation.
    fn enter_sandbox(&mut self, _name: &str, _fresh: bool) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Exit the innermost `in_sandbox` group, discarding its isolated instance and
    /// returning the outer one to the active position. The default is a no-op.
    fn exit_sandbox(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Route a lowered request to the matching method. The default routing is
    /// fixed by [`Request`]; a driver overrides individual methods, not this.
    fn dispatch(&mut self, request: &Request) -> Result<Observation, Self::Error> {
        match request {
            Request::Connect(r) => self.connect(r.clone()),
            Request::Disconnect(c) => self.disconnect(c),
            Request::Call(r) => self.call(r.clone()),
            Request::Watch(r) => self.watch(r.clone()),
            Request::Unwatch(id) => self.unwatch(id),
            Request::ReadView(id) => self.read_view(id),
            Request::AdvanceTime(d) => self.advance_time(d),
            Request::Restart => self.restart(),
            Request::Op(op) => self.op(op),
        }
    }
}

/// The step kinds the engine treats as structural control flow rather than a
/// leaf request to dispatch: their nested programs are run in place.
#[must_use]
pub fn is_structural(kind: &StepKind) -> bool {
    matches!(kind, StepKind::Concurrently | StepKind::InSandbox)
}
