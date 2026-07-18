//! The call and subscription pipelines (SPEC.md §12.1 request pipeline).
//!
//! A `call` runs the §12.1 pipeline: resolve the target, select and verify the
//! authenticator, evaluate role membership at admission, deduplicate by operation
//! identifier (§12.3), commit atomically, and advance the calling connection's
//! subscriptions through the commit before returning (§12.3, §12.6). A `view`
//! opens a subscription with a complete initial result at the connection's
//! frontier (§12.2).

use std::collections::BTreeMap;

use liasse_runtime::{CallOutcome, CallRequest, CommitSeq, Rejection, RejectionReason, Value, ViewQuery};
use liasse_store::InstanceStore;

use crate::authn::AuthContext;
use crate::binding::CallBinding;
use crate::connection::{Connection, DEFAULT_CONTEXT};
use crate::operation::{Dedup, OperationKey, RequestModel};
use crate::outcome::{Denial, DenialReason, SurfaceOutcome};
use crate::reader::EngineReader;
use crate::request::{AuthSelection, SurfaceCall, SurfaceResume, SurfaceWatch};
use crate::role::Role;
use crate::router::Resolved;
use crate::watch::{Watch, WatchAuthz};
use crate::window::{Window, WindowError};

use super::barrier::Barrier;
use super::{Subscription, SurfaceError, SurfaceHost};

impl<S: InstanceStore> SurfaceHost<S> {
    /// Invoke a surface mutation on connection `id` (§12.1).
    ///
    /// # Errors
    /// [`SurfaceError::NoConnection`] if `id` is not open; a store fault from
    /// admission or the barrier sweep. Every §10/§11/§12 refusal is an outcome,
    /// not an error.
    pub fn call(&mut self, id: &str, call: &SurfaceCall) -> Result<SurfaceOutcome, SurfaceError> {
        if !self.connections.contains_key(id) {
            return Err(SurfaceError::NoConnection(id.to_owned()));
        }
        let (binding, context) = match self.resolve_call(id, call) {
            Ok(pair) => pair,
            Err(denial) => return Ok(SurfaceOutcome::Denied(denial)),
        };
        let (request, model) = match Self::build_request(&binding, call.args(), context.as_ref()) {
            Ok(pair) => pair,
            Err(rejection) => return Ok(SurfaceOutcome::Rejected(rejection)),
        };

        let auth_name = context.as_ref().map(|c| c.auth_name().to_owned());
        let op_key = call
            .operation_id()
            .map(|opid| OperationKey::new(call.address().surface_prefix(), auth_name.clone(), opid));
        if let Some(key) = &op_key {
            // Decide, then drop the borrow of `self.operations` (the replay path
            // needs `&mut self` to settle the connection).
            let decision = match self.operations.decide(key, &model) {
                Dedup::Fresh => None,
                Dedup::Replay(outcome) => Some(Ok(outcome.clone())),
                Dedup::Conflict => Some(Err(())),
            };
            match decision {
                Some(Ok(replayed)) => return self.replay(id, replayed),
                Some(Err(())) => {
                    return Ok(SurfaceOutcome::Rejected(Rejection::new(
                        RejectionReason::Malformed,
                        "operation identifier reused with different request metadata",
                    )));
                }
                None => {}
            }
        }

        let outcome = self.execute(id, &request)?;
        if let Some(key) = op_key {
            self.operations.record(key, model, outcome.clone());
        }
        Ok(outcome)
    }

    /// Re-observe a retained outcome for an equivalent retry (§12.3 at-most-once).
    /// The transition is not re-executed, but the §12.3 completion guarantee still
    /// binds the *replaying* connection: receiving `committed` must prove its own
    /// authorized live results have advanced through that commit. So a replayed
    /// commit settles this connection's frontier and sweeps its subscriptions,
    /// exactly as a fresh commit would, before the stored outcome is returned.
    fn replay(&mut self, id: &str, outcome: SurfaceOutcome) -> Result<SurfaceOutcome, SurfaceError> {
        if let Some(commit) = outcome.commit() {
            self.settle_commit(id, commit)?;
        }
        Ok(outcome)
    }

    /// Admit `request` and settle its effect on connection `id`'s frontier and
    /// subscriptions (§12.1 steps 7–8).
    fn execute(&mut self, id: &str, request: &CallRequest) -> Result<SurfaceOutcome, SurfaceError> {
        match self.engine.call(request, &mut self.clock)? {
            CallOutcome::Committed { seq, response } => {
                let frontier = self.settle_commit(id, seq)?;
                Ok(SurfaceOutcome::Committed { frontier, commit: seq, response })
            }
            CallOutcome::Unchanged { response } => {
                let frontier = self
                    .connections
                    .get(id)
                    .map_or_else(|| self.engine.head(), Connection::frontier);
                Ok(SurfaceOutcome::Unchanged { frontier, response })
            }
            CallOutcome::Rejected(rejection) => Ok(SurfaceOutcome::Rejected(rejection)),
        }
    }

    /// Settle commit `seq` on connection `id` (§12.3 step 8): advance the calling
    /// connection's frontier to at least `seq` and sweep its still-authorized
    /// subscriptions, returning that frontier. Every other connection's commit is
    /// an *outgoing frontier* too — §12.2 re-evaluates authority at every outgoing
    /// frontier and emits `close` when state removes a subscription's authority — so
    /// a commit that revokes a peer's membership closes that peer's subscriptions
    /// here as well. What a peer commit does *not* do is advance a peer's rows:
    /// §12.3's completion barrier drags a subscription's row frontier only on its
    /// own connection, so a peer sees new rows no sooner than its own next commit
    /// (the at-least-own-commit frontier guarantee). Peers therefore get an
    /// authority-only sweep, the caller the full one. Sweeping at the connection
    /// frontier (never below it) keeps a subscription that already led `seq` from
    /// regressing to a stale position.
    fn settle_commit(&mut self, id: &str, seq: CommitSeq) -> Result<CommitSeq, SurfaceError> {
        let now = self.clock.instant();
        let barrier = Barrier::new(&self.engine, &self.router, now);
        let mut caller_frontier = seq;
        for (conn_id, connection) in &mut self.connections {
            if conn_id == id {
                connection.advance_frontier(seq);
                caller_frontier = connection.frontier();
                barrier.sweep(connection, caller_frontier)?;
            } else {
                barrier.close_lost_authority(connection)?;
            }
        }
        Ok(caller_frontier)
    }

    /// Resolve a call's target binding and the authenticated context that admitted
    /// it (`None` for a public call — no actor is introduced, §11.1). The context
    /// carries the resolved `$actor`/`$session` the runtime binds for the program.
    fn resolve_call(&self, id: &str, call: &SurfaceCall) -> Result<(CallBinding, Option<AuthContext>), Denial> {
        match self.router.resolve(call.address())? {
            Resolved::PublicCall(binding) => Ok((binding.clone(), None)),
            Resolved::RoleCall { role, binding } => {
                let selection = self.call_selection(id, call)?;
                let now = self.clock.instant();
                let reader = EngineReader::new(&self.engine, now);
                let context = self.authorize_role(role, &selection, &reader)?;
                Ok((binding.clone(), Some(context)))
            }
            Resolved::PublicView(_) | Resolved::RoleView { .. } => {
                Err(Denial::new(DenialReason::Unresolved, "the address targets a view, not a call"))
            }
        }
    }

    /// The selection a role call uses: its per-request `auth`, or the connection's
    /// stored context; denied unauthenticated when neither is present (§11.4).
    fn call_selection(&self, id: &str, call: &SurfaceCall) -> Result<AuthSelection, Denial> {
        if let Some(selection) = call.auth() {
            return Ok(selection.clone());
        }
        let Some(connection) = self.connections.get(id) else {
            return Err(Denial::new(DenialReason::Unauthenticated, "the connection is not open"));
        };
        match connection.select_context(call.context()) {
            Some(selection) => Ok(selection.clone()),
            None => Err(Denial::new(
                DenialReason::Unauthenticated,
                "a role surface requires an authenticated actor",
            )),
        }
    }

    /// Verify a selection and confirm role membership at admission (§10.3, §11.4),
    /// returning the fully resolved [`AuthContext`] (actor, session, authenticator)
    /// so the caller can bind `$actor`/`$session` for the admitted program (§11.1).
    pub(super) fn authorize_role(
        &self,
        role: &Role,
        selection: &AuthSelection,
        reader: &EngineReader<'_, S>,
    ) -> Result<AuthContext, Denial> {
        let context = self.verify_selection(role, selection, reader)?;
        let member = role
            .holds(context.actor().key(), reader)
            .map_err(|_| Denial::new(DenialReason::NotAMember, "membership is unreadable"))?;
        if member {
            Ok(context)
        } else {
            Err(Denial::new(DenialReason::NotAMember, "the actor is not a member of the role"))
        }
    }

    /// Build the runtime [`CallRequest`] (bound receiver + parameters) and the
    /// §12.3 request model (the full verbatim arguments) for dedup equivalence.
    pub(super) fn build_request(
        binding: &CallBinding,
        args: &BTreeMap<String, Value>,
        context: Option<&AuthContext>,
    ) -> Result<(CallRequest, RequestModel), Rejection> {
        let mut request = CallRequest::new(binding.mutation());
        // §11.1/§11.3: an authenticated call carries its resolved `$actor` (and
        // `$session`, when the authenticator declared one) so the runtime binds
        // them for the program. A public call carries neither.
        if let Some(context) = context {
            request = request.actor(context.actor().key().clone());
            if let Some(session) = context.session() {
                request = request.session(session.key().clone());
            }
        }
        let mut receiver = Vec::new();
        for name in binding.receiver() {
            let Some(value) = args.get(name) else {
                return Err(Rejection::new(
                    RejectionReason::Malformed,
                    format!("missing receiver argument `{name}`"),
                ));
            };
            request = request.receiver(value.clone());
            receiver.push(value.clone());
        }
        // A declared parameter the caller omitted is not bound here: the runtime
        // binds an absent optional parameter to `none` (§8.3/§A.1) and rejects an
        // omitted required one (`collect_params`). Enforcing presence at the
        // surface would wrongly deny a call that clears an optional field by
        // omission, so pass omitted parameters through untouched.
        for name in binding.params() {
            if let Some(value) = args.get(name) {
                request = request.arg(name.clone(), value.clone());
            }
        }
        let model = RequestModel::new(binding.mutation(), receiver, args.clone());
        Ok((request, model))
    }

    /// Open a live subscription over a surface view on connection `id`, optionally
    /// bounded by a window (§12.2).
    ///
    /// # Errors
    /// [`SurfaceError::NoConnection`] if `id` is not open; a store fault from
    /// evaluating the view.
    pub fn watch(&mut self, id: &str, watch: &SurfaceWatch) -> Result<Subscription, SurfaceError> {
        if !self.connections.contains_key(id) {
            return Err(SurfaceError::NoConnection(id.to_owned()));
        }
        let (view_name, authz, context) =
            match self.resolve_view(id, watch.address(), watch.context(), watch.auth()) {
                Ok(triple) => triple,
                Err(denial) => return Ok(Subscription::Denied(denial)),
            };
        let frontier = self.connection_frontier(id);
        let query = view_query(watch.args().clone(), context.as_ref());
        self.open_subscription(
            id,
            watch.id(),
            view_name,
            authz,
            frontier,
            watch.window().cloned(),
            watch.args().clone(),
            &query,
        )
    }

    /// Resume subscription `resume` on connection `id` from a retained frontier
    /// (§12.2): re-authorize, then reconstruct the authorized declared view at the
    /// current frontier as a fresh `init`. Membership is re-evaluated, so a resume
    /// that has since lost authority is refused before any row flows.
    ///
    /// # Errors
    /// [`SurfaceError::NoConnection`] if `id` is not open; a store fault from
    /// evaluating the view.
    pub fn resume(&mut self, id: &str, resume: &SurfaceResume) -> Result<Subscription, SurfaceError> {
        if !self.connections.contains_key(id) {
            return Err(SurfaceError::NoConnection(id.to_owned()));
        }
        let (view_name, authz, context) =
            match self.resolve_view(id, resume.address(), resume.context(), resume.auth()) {
                Ok(triple) => triple,
                Err(denial) => return Ok(Subscription::Denied(denial)),
            };
        // The retained `from` is a resume hint; this implementation always
        // reconstructs a fresh init at the connection's current frontier, which
        // covers `from` and yields the current authorized result (§12.2). A resume
        // continues the same stream, so it carries the surface `$params` the
        // original subscription was opened with (§10.1): re-binding them keeps a
        // parameterized subscription on its filtered result instead of collapsing
        // to declared parameter defaults (§8.3).
        let frontier = self.connection_frontier(id);
        let query = view_query(resume.args().clone(), context.as_ref());
        self.open_subscription(
            id,
            resume.id(),
            view_name,
            authz,
            frontier,
            None,
            resume.args().clone(),
            &query,
        )
    }

    /// The current frontier of connection `id`, or the engine head if it is gone.
    fn connection_frontier(&self, id: &str) -> liasse_runtime::CommitSeq {
        self.connections.get(id).map_or_else(|| self.engine.head(), Connection::frontier)
    }

    /// Evaluate the resolved view at `frontier`, open a subscription (bounded by
    /// `window` when present), install it on connection `id`, and report the
    /// initial result (§12.2).
    #[allow(clippy::too_many_arguments)]
    fn open_subscription(
        &mut self,
        id: &str,
        watch_id: &str,
        view_name: String,
        authz: WatchAuthz,
        frontier: liasse_runtime::CommitSeq,
        window: Option<Window>,
        args: BTreeMap<String, Value>,
        query: &ViewQuery,
    ) -> Result<Subscription, SurfaceError> {
        // §10.1: a parameterized `$view` and a role `$view` reading `$actor` are
        // served through the param- and actor-aware read; a plain public view
        // supplies an empty query, matching the argument-free read.
        let Some(result) = self.engine.view_with(&view_name, frontier, query)? else {
            return Ok(Subscription::Denied(Denial::new(
                DenialReason::Unresolved,
                "the surface view is not declared",
            )));
        };
        match window {
            Some(window) => {
                // §7.5/§12.2: a scalar/aggregate view delivers a single value, not a
                // row stream, so there is nothing for a bounded window to bound.
                // Refuse the window (like an absent anchor) rather than present the
                // value as a lossy empty window — the client can subscribe unwindowed,
                // which delivers the scalar.
                if result.scalar().is_some() {
                    return Ok(Subscription::Failed(WindowError::ScalarView));
                }
                let mut opened = Watch::windowed(view_name, authz, frontier, window).with_args(args);
                if let Err(error) = opened.init(result, frontier) {
                    return Ok(Subscription::Failed(error));
                }
                let rows = opened.window_rows().unwrap_or_default().to_vec();
                self.install_watch(id, watch_id, opened);
                Ok(Subscription::Window(rows))
            }
            None => {
                let mut opened = Watch::open(view_name, authz, frontier).with_args(args);
                let _ = opened.init(result.clone(), frontier);
                self.install_watch(id, watch_id, opened);
                Ok(Subscription::Init(result))
            }
        }
    }

    /// Install an opened subscription `watch` as `watch_id` on connection `id`.
    fn install_watch(&mut self, id: &str, watch_id: &str, watch: Watch) {
        if let Some(connection) = self.connections.get_mut(id) {
            connection.insert_watch(watch_id.to_owned(), watch);
        }
    }

    /// Resolve a subscription's view and authorization context (§12.2). A role
    /// subscription authorizes from its per-request `auth` selection (§11.4) when
    /// one is supplied, otherwise from the connection's stored context.
    fn resolve_view(
        &self,
        id: &str,
        address: &crate::address::SurfaceAddress,
        context: Option<&str>,
        selection: Option<&AuthSelection>,
    ) -> Result<(String, WatchAuthz, Option<AuthContext>), Denial> {
        match self.router.resolve(address)? {
            Resolved::PublicView(binding) => {
                Ok((binding.view().to_owned(), WatchAuthz::public(), None))
            }
            Resolved::RoleView { role, binding } => {
                // §11.4: a per-request `auth` selection admits the subscription
                // without a connection-stored context; otherwise fall back to the
                // context the connection bound at `authenticate`.
                let inline = selection.cloned();
                let selection = match inline.clone() {
                    Some(selection) => selection,
                    None => {
                        let Some(connection) = self.connections.get(id) else {
                            return Err(Denial::new(
                                DenialReason::Unauthenticated,
                                "the connection is not open",
                            ));
                        };
                        match connection.select_context(context).cloned() {
                            Some(selection) => selection,
                            None => {
                                return Err(Denial::new(
                                    DenialReason::Unauthenticated,
                                    "a role surface requires an authenticated actor",
                                ));
                            }
                        }
                    }
                };
                let now = self.clock.instant();
                let reader = EngineReader::new(&self.engine, now);
                // §11.1/§11.3: resolve `$actor`/`$session` so a role `$view`
                // reading them is served the authenticated identity, not the
                // unbound (fail-closed) read.
                let auth_context = self.authorize_role(role, &selection, &reader)?;
                let context = context.unwrap_or(DEFAULT_CONTEXT).to_owned();
                let mut authz = WatchAuthz::role(context, role.name().to_owned());
                // §12.2: a subscription opened under a per-request selection
                // re-authorizes from that credential at every frontier, since no
                // connection context backs it.
                if let Some(inline) = inline {
                    authz = authz.with_selection(inline);
                }
                Ok((binding.view().to_owned(), authz, Some(auth_context)))
            }
            Resolved::PublicCall(_) | Resolved::RoleCall { .. } => Err(Denial::new(
                DenialReason::Unresolved,
                "the address targets a call, not a view",
            )),
        }
    }
}

/// Build the runtime [`ViewQuery`] a subscription evaluates its `$view` under: the
/// surface `$params` arguments (§10.1) plus, for a role subscription, the resolved
/// `$actor`/`$session` identity (§11.1/§11.3). A public subscription with no
/// parameters yields the empty query — the argument-free read.
pub(crate) fn view_query(args: BTreeMap<String, Value>, context: Option<&AuthContext>) -> ViewQuery {
    let mut query = ViewQuery::new();
    for (name, value) in args {
        query = query.param(name, value);
    }
    if let Some(context) = context {
        query = query.actor(context.actor().key().clone());
        if let Some(session) = context.session() {
            query = query.session(session.key().clone());
        }
    }
    query
}
