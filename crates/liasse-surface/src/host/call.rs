//! The call and subscription pipelines (SPEC.md §12.1 request pipeline).
//!
//! A `call` runs the §12.1 pipeline: resolve the target, select and verify the
//! authenticator, evaluate role membership at admission, deduplicate by operation
//! identifier (§12.3), commit atomically, and advance the calling connection's
//! subscriptions through the commit before returning (§12.3, §12.6). A `view`
//! opens a subscription with a complete initial result at the connection's
//! frontier (§12.2).

use std::collections::BTreeMap;

use liasse_runtime::{
    CallOutcome, CallRequest, CommitSeq, Rejection, RejectionReason, ScopedReceiver, ScopedResolution,
    Value, ViewQuery,
};
use liasse_store::InstanceStore;

use crate::address::{Authority, SurfaceAddress};
use crate::authn::AuthContext;
use crate::binding::CallBinding;
use crate::connection::DEFAULT_CONTEXT;
use crate::operation::{Dedup, OperationKey, RequestModel};
use crate::outcome::{Denial, DenialReason, SurfaceOutcome};
use crate::reader::EngineReader;
use crate::request::{AuthSelection, SurfaceCall, SurfaceResume, SurfaceWatch};
use crate::role::Role;
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
        let (binding, context, receiver) = match self.resolve_call(id, call) {
            Ok(triple) => triple,
            Err(outcome) => return Ok(outcome),
        };
        let (request, model) =
            match Self::build_request(&binding, call.args(), context.as_ref(), receiver.as_ref()) {
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

    /// Decide the authorization disposition of `call` WITHOUT executing it, so a
    /// boundary can settle the denied/allowed outcome from name resolution and
    /// membership *before* it applies any closed-shape argument check (§10.4,
    /// §12.1, SPEC-ISSUES item 8).
    ///
    /// Runs exactly the [`resolve_call`](Self::resolve_call) pipeline `call` runs —
    /// resolve the target, verify the selection, confirm membership — but
    /// read-only: no admission, no commit, no state change. The arguments are never
    /// read (resolution and membership do not depend on them), so the disposition
    /// is independent of the argument payload. Reports the refusal a caller
    /// observes, or that the caller has established authority:
    ///
    /// * `Ok(())` — the caller is a confirmed member of (or holds public access to)
    ///   the resolved target; a closed-shape argument reveal (`malformed`, the
    ///   declared argument set/types) is now safe to surface to it (item 6).
    /// * `Err(Denied)` — a non-member, an unresolvable name, or an unverified
    ///   selection; a non-member and a nonexistent name are indistinguishable
    ///   (item 8), whatever the argument payload.
    /// * `Err(Rejected)` — a public address carrying an authenticator selection it
    ///   must not carry (§10.2/§11.4); a public surface is enumerable by design, so
    ///   this discloses nothing a `manifest` does not.
    ///
    /// A boundary that gates its argument decode on this closes the enumeration
    /// oracle where a declared-arg-shaped probe to an existing ungranted call would
    /// `Denied` while the same probe to a nonexistent one `Rejected`, revealing
    /// existence by outcome class.
    pub fn authorize_call(&self, id: &str, call: &SurfaceCall) -> Result<(), SurfaceOutcome> {
        self.resolve_call(id, call).map(|_| ())
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
                let frontier = match self.connections.get(id) {
                    Some(connection) => connection.frontier(),
                    None => self.engine.head()?,
                };
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
        let mut barrier = Barrier::new(&self.engine, &self.router, now);
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
    ///
    /// A refusal is returned as the [`SurfaceOutcome`] the caller reports: a
    /// resolution or authorization failure is `Denied`, while a public request that
    /// carries an authenticator selection it must not carry is `Rejected`
    /// (malformed, §10.2/§11.4).
    ///
    /// For a role call the pipeline resolves the role, verifies the selection, and
    /// confirms membership *before* the specific surface/call binding is resolved
    /// (SPEC-ISSUES item 8): a caller who is not a confirmed member never learns
    /// whether the named surface or call exists, so an ungranted surface is
    /// indistinguishable from a nonexistent one. An *unauthenticated* role call — a
    /// role that exists but no actor is bound — is likewise collapsed to the uniform
    /// unresolvable-name denial ([`Self::hide_unenumerable_denial`], §10.4), so it
    /// does not leak that the role exists where a nonexistent role would deny
    /// `unresolved`.
    fn resolve_call(
        &self,
        id: &str,
        call: &SurfaceCall,
    ) -> Result<(CallBinding, Option<AuthContext>, Option<ScopedReceiver>), SurfaceOutcome> {
        match call.address().authority() {
            Authority::Public => {
                let binding = self.router.public_call(call.address()).map_err(SurfaceOutcome::Denied)?;
                // §10.2/§11.4 (SPEC-ISSUES item 8): a public address carries no
                // authenticator selection. A public request that nonetheless
                // attaches one is malformed — rejected here, never dropped and
                // served actor-less.
                if call.auth().is_some() {
                    return Err(SurfaceOutcome::Rejected(Rejection::new(
                        RejectionReason::Malformed,
                        "a public address carries no authenticator selection",
                    )));
                }
                Ok((binding.clone(), None, None))
            }
            Authority::Role(role) => {
                let role_def =
                    self.router.role(role).ok_or_else(|| SurfaceOutcome::Denied(Self::unresolved_name()))?;
                // §10.4: an actor-required denial over this (unenumerable) role is
                // collapsed to `unresolved`, so an anonymous caller cannot tell an
                // existing role from a nonexistent one by the wire code.
                let selection = self.call_selection(id, call).map_err(|denial| {
                    SurfaceOutcome::Denied(Self::hide_unenumerable_denial(call.address().authority(), denial))
                })?;
                let now = self.clock.instant();
                let reader = EngineReader::new(&self.engine, now);
                // §10.3/§10.5: membership is confirmed for the SCOPE the request
                // addresses, so a holder scoped to one row is not admitted to another.
                let context = self
                    .authorize_role(role_def, &selection, call.scope(), &reader)
                    .map_err(SurfaceOutcome::Denied)?;
                // Member confirmed: only now may the surface/call binding's
                // existence be revealed (SPEC-ISSUES item 8).
                let binding = self.router.role_call(role, call.address()).map_err(SurfaceOutcome::Denied)?;
                // §10.5: resolve the addressed receiver — the role-holding row, or a
                // covered descendant whose whole key path re-walks the recursive
                // relation (strict, `$where`-included, non-`$except` at every step). A
                // scope that names no live row or a non-included descendant collapses
                // to the same uniform unresolvable-name denial as a nonexistent
                // address (§10.4), so a bad key path is no oracle.
                let receiver = self
                    .resolve_scoped_receiver(call, context.actor().key())
                    .map_err(SurfaceOutcome::Denied)?;
                Ok((binding.clone(), Some(context), receiver))
            }
        }
    }

    /// Resolve the receiver a scoped-role addressed call mutates (§10.5): the
    /// role-holding row keyed by the request scope, or a covered descendant addressed
    /// by its key path down through `$field`/`$through`. Returns `Ok(None)` when the
    /// address is not a scoped-role surface — an ordinary call whose receiver comes
    /// from its arguments — and `Err(unresolved)` when the scope names no live row or
    /// a descendant step is not a strict, `$where`-included, non-`$except` descendant,
    /// indistinguishable from a nonexistent address (§10.4). A store fault fails
    /// closed to the same denial.
    fn resolve_scoped_receiver(
        &self,
        call: &SurfaceCall,
        actor: &Value,
    ) -> Result<Option<ScopedReceiver>, Denial> {
        // A store fault reading the head fails closed to the uniform
        // unresolvable-name denial, exactly as a `Denied`/`Err` resolution does.
        let frontier = self.engine.head().map_err(|_| Self::unresolved_name())?;
        let resolution = self.engine.scoped_receiver(
            &call.address().surface_prefix(),
            frontier,
            Some(actor),
            call.scope(),
            call.descendant(),
        );
        match resolution {
            Ok(ScopedResolution::Unscoped) => Ok(None),
            Ok(ScopedResolution::Receiver(receiver)) => Ok(Some(receiver)),
            Ok(ScopedResolution::Denied) | Err(_) => Err(Self::unresolved_name()),
        }
    }

    /// The uniform unresolvable-name denial (§10.4, §12.1): one `denied` outcome
    /// for every name a caller is not authorized to have served, so a nonexistent
    /// name is indistinguishable from an ungranted one (SPEC-ISSUES item 8).
    fn unresolved_name() -> Denial {
        Denial::new(DenialReason::Unresolved, "the address names nothing exposed to this caller")
    }

    /// Collapse an actor-required denial over a target the caller cannot enumerate
    /// to the uniform unresolvable-name denial (§10.4).
    ///
    /// [`DenialReason::Unauthenticated`] is *not* name-independent: it fires only
    /// after a role's existence is confirmed — an existing role passes the
    /// role-existence check and reaches the actor check, while a nonexistent role
    /// short-circuits to [`DenialReason::Unresolved`]. Emitting `unauthenticated`
    /// for an [`Authority::Role`] target would therefore let an *unauthenticated*
    /// caller enumerate the role catalog by wire code — `member.x` (exists) denying
    /// `unauthenticated` while `ghost.x` (absent) denies `unresolved`. For a role
    /// (unenumerable) target the denial is remapped to the uniform unresolvable-name
    /// outcome, identical in class, code, and message to a nonexistent name
    /// ([`Self::unresolved_name`]).
    ///
    /// An [`Authority::Public`] target is enumerable via `manifest`, so an
    /// actor-required denial over it would disclose nothing and is preserved. (A
    /// public surface can never in fact require an actor — a `$actor`/`$session`
    /// read inside a public program is rejected at load, and an indirect one faults
    /// at admission as a `rejected`, never a `denied`, §10.2 — so the preserved
    /// branch guards a structural invariant; the remap stays predicated on authority
    /// so a caller who *may* enumerate the target still reads the precise reason.)
    fn hide_unenumerable_denial(authority: &Authority, denial: Denial) -> Denial {
        if matches!(authority, Authority::Role(_)) && denial.reason() == DenialReason::Unauthenticated {
            Self::unresolved_name()
        } else {
            denial
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
        scope: &[Value],
        reader: &EngineReader<'_, S>,
    ) -> Result<AuthContext, Denial> {
        let context = self.verify_selection(role, selection, reader)?;
        // §10.3/§12.1 (SPEC-ISSUES item 8): a non-member — and an unreadable
        // membership, fail-closed — denies as the uniform unresolvable-name
        // outcome, indistinguishable (class and diagnostic code) from a name that
        // does not exist, so a non-member cannot enumerate the role's surfaces.
        // §10.3/§10.5: for a scoped role the membership is confirmed for the exact
        // scope row the request addresses, so a holder scoped to another row denies
        // with the same uniform outcome — no cross-scope grant.
        let member = role
            .holds(context.actor().key(), scope, reader)
            .map_err(|_| Self::unresolved_name())?;
        if member {
            Ok(context)
        } else {
            Err(Self::unresolved_name())
        }
    }

    /// Build the runtime [`CallRequest`] (bound receiver + parameters) and the
    /// §12.3 request model (the full verbatim arguments) for dedup equivalence.
    pub(super) fn build_request(
        binding: &CallBinding,
        args: &BTreeMap<String, Value>,
        context: Option<&AuthContext>,
        scoped: Option<&ScopedReceiver>,
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
        match scoped {
            // §10.3/§10.5: a scoped-role call binds the receiver from the addressed
            // row identity — the role-holding row keyed by the request scope, or the
            // covered descendant its key path resolved to — not from the call
            // arguments. The descendant addresses a row below the mutation's declared
            // collection, so the request also carries that receiver path override.
            Some(scoped) => {
                for component in &scoped.key {
                    request = request.receiver(component.clone());
                    receiver.push(component.clone());
                }
                request = request.receiver_path(scoped.path.clone());
            }
            None => {
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
            }
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
            match self.resolve_view(id, watch.address(), watch.context(), watch.auth(), watch.scope()) {
                Ok(triple) => triple,
                Err(denial) => return Ok(Subscription::Denied(denial)),
            };
        let frontier = self.connection_frontier(id)?;
        let query = view_query(watch.args().clone(), context.as_ref(), watch.scope());
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
        // A resume reconstructs the same stream (§12.2); this phase carries no scope
        // key on a resume, so an unscoped read is rebuilt (empty scope).
        let (view_name, authz, context) =
            match self.resolve_view(id, resume.address(), resume.context(), resume.auth(), &[]) {
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
        let frontier = self.connection_frontier(id)?;
        // A resume reconstructs the same stream (§12.2); scoped-role resume carries
        // no scope key this phase, so an unscoped read is rebuilt (empty scope).
        let query = view_query(resume.args().clone(), context.as_ref(), &[]);
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
    fn connection_frontier(&self, id: &str) -> Result<liasse_runtime::CommitSeq, SurfaceError> {
        match self.connections.get(id) {
            Some(connection) => Ok(connection.frontier()),
            None => Ok(self.engine.head()?),
        }
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
        // §10.4: a scoped covered `$view` that materializes no row (an empty/absent
        // scope, or a fault-closed absent view, §6.3) must deny with the SAME uniform
        // unresolvable-name outcome as a nonexistent address — never a bespoke,
        // distinguishable message. Routing through `unresolved_name` keeps the
        // view/watch path from ever minting a message an enumeration probe could read.
        let Some(result) = self.engine.view_with(&view_name, frontier, query)? else {
            return Ok(Subscription::Denied(Self::unresolved_name()));
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
                let mut opened = Watch::windowed(view_name, authz, frontier, window)
                    .with_args(args)
                    .with_scope(query.scope_key().to_vec());
                if let Err(error) = opened.init(result, frontier) {
                    return Ok(Subscription::Failed(error));
                }
                let rows = opened.window_rows().unwrap_or_default().to_vec();
                self.install_watch(id, watch_id, opened);
                Ok(Subscription::Window(rows))
            }
            None => {
                let mut opened = Watch::open(view_name, authz, frontier)
                    .with_args(args)
                    .with_scope(query.scope_key().to_vec());
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
        address: &SurfaceAddress,
        context: Option<&str>,
        selection: Option<&AuthSelection>,
        scope: &[Value],
    ) -> Result<(String, WatchAuthz, Option<AuthContext>), Denial> {
        match address.authority() {
            Authority::Public => {
                let binding = self.router.public_view(address)?;
                Ok((binding.view().to_owned(), WatchAuthz::public(), None))
            }
            Authority::Role(role) => {
                // §12.2 (SPEC-ISSUES item 8): resolve the role, verify the
                // selection, and confirm membership before the surface view's
                // existence is revealed, so a non-member cannot enumerate a role's
                // views. A nonexistent role, a non-member, and an unauthenticated
                // role read all deny as the uniform unresolvable-name outcome (the
                // actor-required denials below pass through
                // [`Self::hide_unenumerable_denial`], §10.4).
                let role_def = self.router.role(role).ok_or_else(Self::unresolved_name)?;
                // §11.4: a per-request `auth` selection admits the subscription
                // without a connection-stored context; otherwise fall back to the
                // context the connection bound at `authenticate`.
                let inline = selection.cloned();
                let selection = match inline.clone() {
                    Some(selection) => selection,
                    None => {
                        let Some(connection) = self.connections.get(id) else {
                            return Err(Self::hide_unenumerable_denial(
                                address.authority(),
                                Denial::new(DenialReason::Unauthenticated, "the connection is not open"),
                            ));
                        };
                        match connection.select_context(context).cloned() {
                            Some(selection) => selection,
                            None => {
                                return Err(Self::hide_unenumerable_denial(
                                    address.authority(),
                                    Denial::new(
                                        DenialReason::Unauthenticated,
                                        "a role surface requires an authenticated actor",
                                    ),
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
                // §10.3/§10.5: a scoped-role subscription is authorized for the exact
                // scope row it names, so a holder scoped to one company cannot watch
                // another company's covered `$view` (the cross-scope read is denied
                // with the same uniform outcome, §10.4).
                let auth_context = self.authorize_role(role_def, &selection, scope, &reader)?;
                let binding = self.router.role_view(role, address)?;
                let context = context.unwrap_or(DEFAULT_CONTEXT).to_owned();
                let mut authz = WatchAuthz::role(context, role_def.name().to_owned());
                // §12.2: a subscription opened under a per-request selection
                // re-authorizes from that credential at every frontier, since no
                // connection context backs it.
                if let Some(inline) = inline {
                    authz = authz.with_selection(inline);
                }
                Ok((binding.view().to_owned(), authz, Some(auth_context)))
            }
        }
    }

    /// Decide the authorization disposition of a subscription over `watch`'s target
    /// WITHOUT opening it, so a boundary can settle the denied/allowed outcome from
    /// name resolution and membership *before* it applies any closed-shape `$params`
    /// check (§10.4, §12.1, GitHub #39 — the `view`/`fetch` mirror of the item-8
    /// `call` oracle [`authorize_call`](Self::authorize_call) closes).
    ///
    /// Runs exactly the [`resolve_view`](Self::resolve_view) pipeline `watch`/`resume`
    /// run — resolve the role, verify the selection, confirm membership — but
    /// read-only: no subscription is installed and no rows flow. The surface `$params`
    /// arguments are never read (resolution and membership do not depend on them), so
    /// the disposition is independent of the params payload:
    ///
    /// * `Ok(())` — the caller is a confirmed member of (or holds public access to)
    ///   the resolved view; a closed-shape `$params` reveal (`malformed`, the declared
    ///   parameter set/types) is now safe to surface to it (item 6/#10).
    /// * `Err(Denial)` — a non-member, an unresolvable name, an unauthenticated role
    ///   read, or an unverified selection; a non-member, a nonexistent name, and an
    ///   unauthenticated role read are indistinguishable (class and diagnostic code),
    ///   whatever the params payload (the actor-required denial collapses to
    ///   `unresolved`, §10.4).
    ///
    /// A boundary that gates its `$params` decode on this closes the enumeration
    /// oracle where a valid-param-shaped probe to an existing ungranted view would
    /// `Denied` while the same probe to a nonexistent one `Rejected`, revealing
    /// existence by outcome class.
    pub fn authorize_view(&self, id: &str, watch: &SurfaceWatch) -> Result<(), Denial> {
        self.resolve_view(id, watch.address(), watch.context(), watch.auth(), watch.scope()).map(|_| ())
    }
}

/// Build the runtime [`ViewQuery`] a subscription evaluates its `$view` under: the
/// surface `$params` arguments (§10.1) plus, for a role subscription, the resolved
/// `$actor`/`$session` identity (§11.1/§11.3) and — for a scoped-role subscription
/// (§10.5) — the scope-row key path the covered `$view` reads `.` as. A public
/// subscription with no parameters yields the empty query — the argument-free read.
pub(crate) fn view_query(
    args: BTreeMap<String, Value>,
    context: Option<&AuthContext>,
    scope: &[Value],
) -> ViewQuery {
    let mut query = ViewQuery::new().scope(scope.iter().cloned());
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
