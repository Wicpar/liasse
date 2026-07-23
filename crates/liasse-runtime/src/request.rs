//! The typed request an external caller submits (§8.5, §10): a named mutation,
//! the key of the selected receiver row (for a row mutation), and typed
//! arguments. Callers provide already-typed [`Value`]s — parse, don't validate.

use std::collections::BTreeMap;

use liasse_value::Value;

/// A mutation call: which operation, which receiver row, and what arguments.
///
/// When the call is admitted through an authenticated external role (§11.1), it
/// also carries the resolved `$actor` and (when the authenticator declared one)
/// `$session` row keys, so the admission environment binds `$actor`/`$session`
/// for a program that reads them (§6.2, §11). A public or internal call carries
/// neither: no actor is introduced (§11.1).
#[derive(Debug, Clone)]
pub struct CallRequest {
    mutation: String,
    receiver: Vec<Value>,
    receiver_path: Option<Vec<String>>,
    args: BTreeMap<String, Value>,
    actor: Option<Value>,
    session: Option<Value>,
}

impl CallRequest {
    /// A call of the mutation named `mutation`.
    #[must_use]
    pub fn new(mutation: impl Into<String>) -> Self {
        Self {
            mutation: mutation.into(),
            receiver: Vec::new(),
            receiver_path: None,
            args: BTreeMap::new(),
            actor: None,
            session: None,
        }
    }

    /// Bind the resolved `$actor` row key admitting this call (§11.1, §11.3): the
    /// key of the application row the authenticator selected as the actor. The
    /// admission environment re-materializes that row so `$actor` resolves in the
    /// mutation program.
    #[must_use]
    pub fn actor(mut self, key: Value) -> Self {
        self.actor = Some(key);
        self
    }

    /// Bind the resolved `$session` row key admitting this call (§11.2, §11.3),
    /// when the selected authenticator declared a `$session`.
    #[must_use]
    pub fn session(mut self, key: Value) -> Self {
        self.session = Some(key);
        self
    }

    /// The resolved `$actor` row key, if this is an authenticated call.
    #[must_use]
    pub fn actor_key(&self) -> Option<&Value> {
        self.actor.as_ref()
    }

    /// The resolved `$session` row key, if the authenticator declared one.
    #[must_use]
    pub fn session_key(&self) -> Option<&Value> {
        self.session.as_ref()
    }

    /// Append a receiver key component (§8.2). A single-field key needs one; a
    /// composite key needs each component in `$key` order.
    #[must_use]
    pub fn receiver(mut self, component: Value) -> Self {
        self.receiver.push(component);
        self
    }

    /// Override the collection declaration path the receiver key addresses (§10.5):
    /// a scoped-role covered-descendant call addresses a row *below* the mutation's
    /// declared collection — `companies[root].subcompanies[a]` for a `companies`
    /// mutation — so the receiver descends this path (in `$key` order across every
    /// level) rather than the mutation's own declared path. Absent for an ordinary
    /// call, which addresses the receiver at the mutation's declared location.
    #[must_use]
    pub fn receiver_path(mut self, path: impl IntoIterator<Item = String>) -> Self {
        self.receiver_path = Some(path.into_iter().collect());
        self
    }

    /// The receiver collection path override (§10.5), if a scoped-role descendant
    /// call set one. `None` addresses the receiver at the mutation's declared path.
    #[must_use]
    pub fn receiver_path_override(&self) -> Option<&[String]> {
        self.receiver_path.as_deref()
    }

    /// Bind a mutation argument `@name` to a typed value (§8.3).
    #[must_use]
    pub fn arg(mut self, name: impl Into<String>, value: Value) -> Self {
        self.args.insert(name.into(), value);
        self
    }

    /// This request retargeted at a different mutation, keeping its arguments,
    /// receiver, and actor/session bindings (§13.10): an interface-addressed
    /// mutation `interface.mutation` routes to the child's private mutation under
    /// the same arguments.
    #[must_use]
    pub fn with_mutation(mut self, mutation: impl Into<String>) -> Self {
        self.mutation = mutation.into();
        self
    }

    /// The mutation name.
    #[must_use]
    pub fn mutation(&self) -> &str {
        &self.mutation
    }

    /// The receiver key components, in `$key` order.
    #[must_use]
    pub fn receiver_key(&self) -> &[Value] {
        &self.receiver
    }

    /// The argument bound to `name`, if supplied.
    #[must_use]
    pub fn arg_value(&self, name: &str) -> Option<&Value> {
        self.args.get(name)
    }

    /// Every supplied argument, by name (§8.3) — the closed argument object a call
    /// carries. Used to type the parameters of an inline `$expose` mutation the
    /// interface prototype supplies (§13.8).
    #[must_use]
    pub fn args(&self) -> &BTreeMap<String, Value> {
        &self.args
    }
}

/// The parameter bindings and actor/session identity a view read runs under
/// (§10.1, §11.1) — the input to [`Engine::view_with`](crate::Engine::view_with).
///
/// A surface `$view` reads its `$params` as `@name` and a role `$view` reads
/// `$actor`/`$session`; both fault unbound when read outside an admission. This
/// query supplies them for a read, mirroring the context an authenticated
/// [`CallRequest`] threads at admission: each `@name` resolves from [`param`], and
/// `$actor`/`$session` resolve from the row the [`actor`]/[`session`] key names,
/// re-materialized from committed state at the read frontier (§10.3, §11.3). A key
/// that resolves no live row leaves that binding unbound, so a view reading it
/// faults closed — fail closed (§6.3).
///
/// [`param`]: ViewQuery::param
/// [`actor`]: ViewQuery::actor
/// [`session`]: ViewQuery::session
#[derive(Debug, Clone, Default)]
pub struct ViewQuery {
    params: BTreeMap<String, Value>,
    actor: Option<Value>,
    session: Option<Value>,
    scope: Vec<Value>,
}

impl ViewQuery {
    /// An empty query: no parameters bound, no actor or session identity. A view
    /// reading `@param` or `$actor` under it faults exactly as an unbound read.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind the view parameter `@name` to a typed value (§10.1). An unbound
    /// declared parameter takes its declared default when the view is evaluated.
    #[must_use]
    pub fn param(mut self, name: impl Into<String>, value: Value) -> Self {
        self.params.insert(name.into(), value);
        self
    }

    /// Bind the resolved `$actor` row key this read runs under (§11.1, §11.3): the
    /// key of the application row the authenticator selected as the actor, so a
    /// role `$view` reading `$actor` resolves it against committed state.
    #[must_use]
    pub fn actor(mut self, key: Value) -> Self {
        self.actor = Some(key);
        self
    }

    /// Bind the resolved `$session` row key this read runs under (§11.2, §11.3),
    /// when the selected authenticator declared a `$session`.
    #[must_use]
    pub fn session(mut self, key: Value) -> Self {
        self.session = Some(key);
        self
    }

    /// The bound view parameters, by name.
    #[must_use]
    pub fn params(&self) -> &BTreeMap<String, Value> {
        &self.params
    }

    /// The resolved `$actor` row key, if this read is authenticated.
    #[must_use]
    pub fn actor_key(&self) -> Option<&Value> {
        self.actor.as_ref()
    }

    /// The resolved `$session` row key, if the authenticator declared one.
    #[must_use]
    pub fn session_key(&self) -> Option<&Value> {
        self.session.as_ref()
    }

    /// Bind the scope-row key path this read runs under (§10.5): the containing
    /// row identity a scoped-role surface is addressed by, in `$key` order (a
    /// single-component key is a one-element path). A scoped-role `$view` reads
    /// `.` as that row; an empty scope leaves the read rooted at the package root.
    #[must_use]
    pub fn scope(mut self, key: impl IntoIterator<Item = Value>) -> Self {
        self.scope = key.into_iter().collect();
        self
    }

    /// The scope-row key path this read runs under (§10.5), in `$key` order.
    /// Empty for an unscoped (public or package-level) read.
    #[must_use]
    pub fn scope_key(&self) -> &[Value] {
        &self.scope
    }
}
