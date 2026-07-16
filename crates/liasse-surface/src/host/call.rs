//! The call and subscription pipelines (SPEC.md §12.1 request pipeline).
//!
//! A `call` runs the §12.1 pipeline: resolve the target, select and verify the
//! authenticator, evaluate role membership at admission, deduplicate by operation
//! identifier (§12.3), commit atomically, and advance the calling connection's
//! subscriptions through the commit before returning (§12.3, §12.6). A `view`
//! opens a subscription with a complete initial result at the connection's
//! frontier (§12.2).

use std::collections::BTreeMap;

use liasse_runtime::{CallOutcome, CallRequest, Rejection, RejectionReason, Value};
use liasse_store::InstanceStore;

use crate::binding::CallBinding;
use crate::connection::{Connection, DEFAULT_CONTEXT};
use crate::operation::{Dedup, OperationKey, RequestModel};
use crate::outcome::{Denial, DenialReason, SurfaceOutcome};
use crate::reader::EngineReader;
use crate::request::{AuthSelection, SurfaceCall, SurfaceWatch};
use crate::role::Role;
use crate::router::Resolved;
use crate::watch::{Watch, WatchAuthz};

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
        let (binding, auth_name) = match self.resolve_call(id, call) {
            Ok(pair) => pair,
            Err(denial) => return Ok(SurfaceOutcome::Denied(denial)),
        };
        let (request, model) = match Self::build_request(&binding, call.args()) {
            Ok(pair) => pair,
            Err(rejection) => return Ok(SurfaceOutcome::Rejected(rejection)),
        };

        let op_key = call
            .operation_id()
            .map(|opid| OperationKey::new(call.address().surface_prefix(), auth_name.clone(), opid));
        if let Some(key) = &op_key {
            match self.operations.decide(key, &model) {
                Dedup::Replay(outcome) => return Ok(outcome.clone()),
                Dedup::Conflict => {
                    return Ok(SurfaceOutcome::Rejected(Rejection::new(
                        RejectionReason::Malformed,
                        "operation identifier reused with different request metadata",
                    )));
                }
                Dedup::Fresh => {}
            }
        }

        let outcome = self.execute(id, &request)?;
        if let Some(key) = op_key {
            self.operations.record(key, model, outcome.clone());
        }
        Ok(outcome)
    }

    /// Admit `request` and settle its effect on connection `id`'s frontier and
    /// subscriptions (§12.1 steps 7–8).
    fn execute(&mut self, id: &str, request: &CallRequest) -> Result<SurfaceOutcome, SurfaceError> {
        match self.engine.call(request, &mut self.clock)? {
            CallOutcome::Committed { seq, response } => {
                let now = self.clock.instant();
                let barrier = Barrier::new(&self.engine, &self.router, now);
                if let Some(connection) = self.connections.get_mut(id) {
                    connection.advance_frontier(seq);
                    barrier.sweep(connection, seq)?;
                }
                Ok(SurfaceOutcome::Committed { commit: seq, response })
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

    /// Resolve a call's target binding and the authenticator that admitted it
    /// (the op-scope authenticator, `None` for a public call).
    fn resolve_call(&self, id: &str, call: &SurfaceCall) -> Result<(CallBinding, Option<String>), Denial> {
        match self.router.resolve(call.address())? {
            Resolved::PublicCall(binding) => Ok((binding.clone(), None)),
            Resolved::RoleCall { role, binding } => {
                let selection = self.call_selection(id, call)?;
                let now = self.clock.instant();
                let reader = EngineReader::new(&self.engine, now);
                let auth_name = self.authorize_role(role, &selection, &reader)?;
                Ok((binding.clone(), Some(auth_name)))
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
    /// returning the admitting authenticator name.
    pub(super) fn authorize_role(
        &self,
        role: &Role,
        selection: &AuthSelection,
        reader: &EngineReader<'_, S>,
    ) -> Result<String, Denial> {
        let context = self.verify_selection(role, selection, reader)?;
        let member = role
            .holds(context.actor().key(), reader)
            .map_err(|_| Denial::new(DenialReason::NotAMember, "membership is unreadable"))?;
        if member {
            Ok(context.auth_name().to_owned())
        } else {
            Err(Denial::new(DenialReason::NotAMember, "the actor is not a member of the role"))
        }
    }

    /// Build the runtime [`CallRequest`] (bound receiver + parameters) and the
    /// §12.3 request model (the full verbatim arguments) for dedup equivalence.
    fn build_request(
        binding: &CallBinding,
        args: &BTreeMap<String, Value>,
    ) -> Result<(CallRequest, RequestModel), Rejection> {
        let mut request = CallRequest::new(binding.mutation());
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
        for name in binding.params() {
            let Some(value) = args.get(name) else {
                return Err(Rejection::new(
                    RejectionReason::Malformed,
                    format!("missing argument `@{name}`"),
                ));
            };
            request = request.arg(name.clone(), value.clone());
        }
        let model = RequestModel::new(binding.mutation(), receiver, args.clone());
        Ok((request, model))
    }

    /// Open a live subscription over a surface view on connection `id` (§12.2).
    ///
    /// # Errors
    /// [`SurfaceError::NoConnection`] if `id` is not open; a store fault from
    /// evaluating the view.
    pub fn watch(&mut self, id: &str, watch: &SurfaceWatch) -> Result<Subscription, SurfaceError> {
        if !self.connections.contains_key(id) {
            return Err(SurfaceError::NoConnection(id.to_owned()));
        }
        let (view_name, authz) = match self.resolve_watch(id, watch) {
            Ok(pair) => pair,
            Err(denial) => return Ok(Subscription::Denied(denial)),
        };
        let frontier = self
            .connections
            .get(id)
            .map_or_else(|| self.engine.head(), Connection::frontier);
        let Some(result) = self.engine.view(&view_name, frontier)? else {
            return Ok(Subscription::Denied(Denial::new(
                DenialReason::Unresolved,
                "the surface view is not declared",
            )));
        };
        let mut opened = Watch::open(view_name, authz, frontier);
        let _ = opened.init(result.clone(), frontier);
        if let Some(connection) = self.connections.get_mut(id) {
            connection.insert_watch(watch.id().to_owned(), opened);
        }
        Ok(Subscription::Init(result))
    }

    /// Resolve a subscription's view and authorization context (§12.2).
    fn resolve_watch(&self, id: &str, watch: &SurfaceWatch) -> Result<(String, WatchAuthz), Denial> {
        match self.router.resolve(watch.address())? {
            Resolved::PublicView(binding) => Ok((binding.view().to_owned(), WatchAuthz::public())),
            Resolved::RoleView { role, binding } => {
                let Some(connection) = self.connections.get(id) else {
                    return Err(Denial::new(DenialReason::Unauthenticated, "the connection is not open"));
                };
                let Some(selection) = connection.select_context(watch.context()).cloned() else {
                    return Err(Denial::new(
                        DenialReason::Unauthenticated,
                        "a role surface requires an authenticated actor",
                    ));
                };
                let now = self.clock.instant();
                let reader = EngineReader::new(&self.engine, now);
                self.authorize_role(role, &selection, &reader)?;
                let context = watch.context().unwrap_or(DEFAULT_CONTEXT).to_owned();
                Ok((binding.view().to_owned(), WatchAuthz::role(context, role.name().to_owned())))
            }
            Resolved::PublicCall(_) | Resolved::RoleCall { .. } => Err(Denial::new(
                DenialReason::Unresolved,
                "the address targets a call, not a view",
            )),
        }
    }
}
