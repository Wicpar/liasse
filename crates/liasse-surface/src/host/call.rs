//! The call and subscription pipelines (SPEC.md §12.1 request pipeline).
//!
//! A `call` runs the §12.1 pipeline: resolve the target, select and verify the
//! authenticator, evaluate role membership at admission, deduplicate by operation
//! identifier (§12.3), commit atomically, and advance the calling connection's
//! subscriptions through the commit before returning (§12.3, §12.6). A `view`
//! opens a subscription with a complete initial result at the connection's
//! frontier (§12.2).

use std::collections::BTreeMap;

use liasse_runtime::{CallOutcome, CallRequest, CommitSeq, Rejection, RejectionReason, Value};
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
use crate::window::Window;

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

    /// Settle commit `seq` on connection `id` (§12.3 step 8): advance its frontier
    /// to at least `seq` and sweep every still-authorized subscription through the
    /// connection's resulting frontier, returning that frontier. Sweeping at the
    /// connection frontier (never below it) keeps a subscription that already led
    /// `seq` — a replay of an older commit, or a connection past this write — from
    /// regressing to a stale position.
    fn settle_commit(&mut self, id: &str, seq: CommitSeq) -> Result<CommitSeq, SurfaceError> {
        let now = self.clock.instant();
        let barrier = Barrier::new(&self.engine, &self.router, now);
        let Some(connection) = self.connections.get_mut(id) else {
            return Ok(seq);
        };
        connection.advance_frontier(seq);
        let frontier = connection.frontier();
        barrier.sweep(connection, frontier)?;
        Ok(frontier)
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
        let (view_name, authz) = match self.resolve_view(id, watch.address(), watch.context()) {
            Ok(pair) => pair,
            Err(denial) => return Ok(Subscription::Denied(denial)),
        };
        let frontier = self.connection_frontier(id);
        self.open_subscription(id, watch.id(), view_name, authz, frontier, watch.window().cloned())
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
        let (view_name, authz) = match self.resolve_view(id, resume.address(), resume.context()) {
            Ok(pair) => pair,
            Err(denial) => return Ok(Subscription::Denied(denial)),
        };
        // The retained `from` is a resume hint; this implementation always
        // reconstructs a fresh init at the connection's current frontier, which
        // covers `from` and yields the current authorized result (§12.2).
        let frontier = self.connection_frontier(id);
        self.open_subscription(id, resume.id(), view_name, authz, frontier, None)
    }

    /// The current frontier of connection `id`, or the engine head if it is gone.
    fn connection_frontier(&self, id: &str) -> liasse_runtime::CommitSeq {
        self.connections.get(id).map_or_else(|| self.engine.head(), Connection::frontier)
    }

    /// Evaluate the resolved view at `frontier`, open a subscription (bounded by
    /// `window` when present), install it on connection `id`, and report the
    /// initial result (§12.2).
    fn open_subscription(
        &mut self,
        id: &str,
        watch_id: &str,
        view_name: String,
        authz: WatchAuthz,
        frontier: liasse_runtime::CommitSeq,
        window: Option<Window>,
    ) -> Result<Subscription, SurfaceError> {
        let Some(result) = self.engine.view(&view_name, frontier)? else {
            return Ok(Subscription::Denied(Denial::new(
                DenialReason::Unresolved,
                "the surface view is not declared",
            )));
        };
        match window {
            Some(window) => {
                let mut opened = Watch::windowed(view_name, authz, frontier, window);
                if let Err(error) = opened.init(result, frontier) {
                    return Ok(Subscription::Failed(error));
                }
                let rows = opened.window_rows().unwrap_or_default().to_vec();
                self.install_watch(id, watch_id, opened);
                Ok(Subscription::Window(rows))
            }
            None => {
                let mut opened = Watch::open(view_name, authz, frontier);
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

    /// Resolve a subscription's view and authorization context (§12.2).
    fn resolve_view(
        &self,
        id: &str,
        address: &crate::address::SurfaceAddress,
        context: Option<&str>,
    ) -> Result<(String, WatchAuthz), Denial> {
        match self.router.resolve(address)? {
            Resolved::PublicView(binding) => Ok((binding.view().to_owned(), WatchAuthz::public())),
            Resolved::RoleView { role, binding } => {
                let Some(connection) = self.connections.get(id) else {
                    return Err(Denial::new(DenialReason::Unauthenticated, "the connection is not open"));
                };
                let Some(selection) = connection.select_context(context).cloned() else {
                    return Err(Denial::new(
                        DenialReason::Unauthenticated,
                        "a role surface requires an authenticated actor",
                    ));
                };
                let now = self.clock.instant();
                let reader = EngineReader::new(&self.engine, now);
                self.authorize_role(role, &selection, &reader)?;
                let context = context.unwrap_or(DEFAULT_CONTEXT).to_owned();
                Ok((binding.view().to_owned(), WatchAuthz::role(context, role.name().to_owned())))
            }
            Resolved::PublicCall(_) | Resolved::RoleCall { .. } => Err(Denial::new(
                DenialReason::Unresolved,
                "the address targets a call, not a view",
            )),
        }
    }
}
