//! Lowering a loaded [`Step`] to a typed [`Request`].
//!
//! The loader keeps a step's action-specific payload verbatim (see [`Step`]);
//! the engine cannot drive a driver from raw JSON. [`Request::lower`] turns each
//! leaf step into a typed request — the core client verbs into their own
//! variants, everything else into a typed [`OpRequest`] tagged by [`StepKind`].
//! Lowering is where "no unknown-step surprise" is proven: every step kind the
//! corpus uses resolves to a request, and a malformed core verb (a bad duration,
//! a missing subscription handle) is a precise [`LowerError`] rather than a
//! silent fall-through.

use serde_json::Value;

use crate::clock::{DurationParseError, Iso8601Duration};
use crate::contract::{CallRequest, ConnectRequest, WatchRequest};
use crate::id::{ConnectionId, WatchId};
use crate::matcher::Bindings;
use crate::step::Step;
use crate::step_kind::StepKind;

/// A step lowered to a typed, driver-ready request.
#[derive(Debug, Clone)]
pub enum Request {
    /// Open a client connection.
    Connect(ConnectRequest),
    /// Close a client connection.
    Disconnect(ConnectionId),
    /// Invoke an external mutation surface.
    Call(CallRequest),
    /// Open a live subscription.
    Watch(WatchRequest),
    /// Close a subscription.
    Unwatch(WatchId),
    /// Read a subscription's current value (`expect_view`).
    ReadView(WatchId),
    /// Advance the virtual clock.
    AdvanceTime(Iso8601Duration),
    /// Stop and replay the runtime.
    Restart,
    /// Any step outside the typed client set.
    Op(OpRequest),
}

/// A typed carrier for every registry and chapter-local step. The kind names the
/// action; `target` and `members` hold its payload with `$ref:` bindings already
/// resolved, so a driver never re-parses the raw step.
#[derive(Debug, Clone)]
pub struct OpRequest {
    /// The action discriminant (`export`, `blob_put`, `module_install`, ...).
    pub kind: StepKind,
    /// The value bound to the action key.
    pub target: Value,
    /// The connection the step runs on, if any.
    pub on: Option<ConnectionId>,
    /// The step's remaining modifiers, with refs resolved.
    pub members: serde_json::Map<String, Value>,
}

impl OpRequest {
    /// The action key text.
    #[must_use]
    pub fn action_key(&self) -> &str {
        self.kind.key()
    }

    /// A member by name.
    #[must_use]
    pub fn member(&self, name: &str) -> Option<&Value> {
        self.members.get(name)
    }
}

/// A step whose typed shape is malformed (independent of any driver).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerError {
    /// The action key of the offending step.
    pub action: String,
    /// What could not be lowered and why.
    pub reason: String,
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot lower `{}` step: {}", self.action, self.reason)
    }
}

impl std::error::Error for LowerError {}

impl From<DurationParseError> for LowerError {
    fn from(err: DurationParseError) -> Self {
        Self { action: "advance_time".to_owned(), reason: err.reason }
    }
}

impl Request {
    /// Lower one leaf step to a typed request, resolving `$ref:` bindings in
    /// outgoing payloads against `env`. Structural steps (`concurrently`,
    /// `in_sandbox`) are handled by the engine and lower to an [`OpRequest`]
    /// that the engine never dispatches.
    pub fn lower(step: &Step, env: &Bindings) -> Result<Self, LowerError> {
        let action = step.action_key();
        match &step.kind {
            StepKind::Connect => Ok(Self::Connect(ConnectRequest {
                connection: ConnectionId::new(require_str(&step.target, action, "connection id")?),
                // §11.4: a login-minted credential (`credential: "$ref:tok1"`) is a
                // bound value like any other payload argument, so resolve `$ref:`
                // bindings inside the authenticate payload before submitting it.
                authenticate: step.member("authenticate").map(|payload| env.resolve(payload)),
            })),
            StepKind::Disconnect => {
                Ok(Self::Disconnect(ConnectionId::new(require_str(&step.target, action, "connection id")?)))
            }
            StepKind::Call => Ok(Self::Call(CallRequest {
                target: require_str(&step.target, action, "surface address")?.to_owned(),
                args: env.resolve(step.member("args").unwrap_or(&Value::Null)),
                on: step.on.clone(),
                operation_id: step.member("operation_id").and_then(Value::as_str).map(ToOwned::to_owned),
                auth: step.member("auth").map(|selection| env.resolve(selection)),
                context: step.member("context").and_then(Value::as_str).map(ToOwned::to_owned),
                // §10.3/§10.5: a scoped-role call names the containing row it is
                // addressed under (`scope`) and, for a covered descendant, its key
                // path down through `$field`/`$through` (`descendant`); both resolve
                // through `env` like any bound payload.
                scope: step.member("scope").map(|scope| env.resolve(scope)),
                descendant: step.member("descendant").map(|descendant| env.resolve(descendant)),
            })),
            StepKind::Watch => Ok(Self::Watch(WatchRequest {
                target: require_str(&step.target, action, "surface address")?.to_owned(),
                id: WatchId::new(require_member_str(step, "id", action)?),
                on: step.on.clone(),
                args: env.resolve(step.member("args").unwrap_or(&Value::Null)),
                window: step.member("window").cloned(),
                auth: step.member("auth").map(|selection| env.resolve(selection)),
                context: step.member("context").and_then(Value::as_str).map(ToOwned::to_owned),
                // §10.5: a scoped-role subscription names the containing row it is
                // addressed under; resolved through `env` like any bound payload.
                scope: step.member("scope").map(|scope| env.resolve(scope)),
            })),
            StepKind::Unwatch => Ok(Self::Unwatch(WatchId::new(require_str(&step.target, action, "subscription id")?))),
            StepKind::ExpectView => Ok(Self::ReadView(WatchId::new(target_member_str(&step.target, "watch", action)?))),
            StepKind::AdvanceTime => Ok(Self::AdvanceTime(Iso8601Duration::parse(require_str(
                &step.target,
                action,
                "ISO-8601 duration",
            )?)?)),
            StepKind::Restart => Ok(Self::Restart),
            _ => Ok(Self::Op(OpRequest {
                kind: step.kind.clone(),
                target: env.resolve(&step.target),
                on: step.on.clone(),
                members: resolve_members(&step.members, env),
            })),
        }
    }
}

fn resolve_members(members: &serde_json::Map<String, Value>, env: &Bindings) -> serde_json::Map<String, Value> {
    members.iter().map(|(k, v)| (k.clone(), env.resolve(v))).collect()
}

fn require_str<'a>(value: &'a Value, action: &str, what: &str) -> Result<&'a str, LowerError> {
    value.as_str().ok_or_else(|| LowerError { action: action.to_owned(), reason: format!("{what} must be a string") })
}

fn require_member_str(step: &Step, name: &str, action: &str) -> Result<String, LowerError> {
    step.member(name)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| LowerError { action: action.to_owned(), reason: format!("`{name}` member must be a string") })
}

fn target_member_str(target: &Value, name: &str, action: &str) -> Result<String, LowerError> {
    target
        .get(name)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| LowerError { action: action.to_owned(), reason: format!("target `{name}` must be a string") })
}
