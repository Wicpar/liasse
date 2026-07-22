//! The call and subscription pipelines (SPEC.md §12.1 request pipeline).
//!
//! A `call` runs the §12.1 pipeline: resolve the target, select and verify the
//! authenticator, evaluate role membership at admission, deduplicate by operation
//! identifier (§12.3), commit atomically, and advance the calling connection's
//! subscriptions through the commit before returning (§12.3, §12.6). A `view`
//! opens a subscription with a complete initial result at the connection's
//! frontier (§12.2).

use std::collections::{BTreeMap, BTreeSet};

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

impl<S: InstanceStore, P: liasse_host::KeyProvider> SurfaceHost<S, P> {
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
        // §12.1: the call's argument object is closed against the resolved
        // mutation's declared parameters — applied here, after membership is
        // confirmed (so a non-member reads the uniform denial, not a distinguishing
        // `malformed`, §10.4) and BEFORE the request/dedup model is built (so no
        // partial effect occurs and the §12.3 dedup identity stays exactly the
        // decoded declared argument set). An undeclared member — including any
        // reserved `$`-prefixed name — is malformed, never silently dropped. This is
        // the `call` mirror of the view path's `closed_view_args`.
        if let Err(rejection) = self.closed_call_args(&binding, call.args()) {
            return Ok(SurfaceOutcome::Rejected(rejection));
        }
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
        // §5.1/§8.12: `now()` is the request-fixed virtual-clock instant (A.5),
        // while the seed `uuid()` derives from is drawn from the CSPRNG entropy —
        // never the monotone clock — so a surface-minted token is unpredictable.
        let now = self.clock.instant();
        let mut generators = self.entropy.generators(now);
        match self.engine.call(request, &mut generators)? {
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
                // §10.4: an actor-absent denial over this (unenumerable) role — no
                // per-request `auth` and no bound context, so no established authority
                // — is collapsed to `unresolved`, so an anonymous caller cannot tell an
                // existing role from a nonexistent one by the wire code.
                let selection = self.call_selection(id, call).map_err(|denial| {
                    SurfaceOutcome::Denied(Self::hide_unenumerable_denial(call.address().authority(), false, denial))
                })?;
                let now = self.clock.instant();
                let reader = EngineReader::new(&self.engine, now);
                // §10.4's exception is TARGET-scoped: precise diagnostics are
                // permitted only toward a caller that has established authority over
                // THE TARGET. A per-request `auth` selection is always a fresh probe
                // (no established authority). A bound connection context is
                // established authority over a role only when it authenticated
                // against that role AND was a verified MEMBER of it at authenticate
                // (§11 auth does not check membership, so a non-member whose session
                // resolves must not be read as established) — never over an unrelated
                // role, and never as a non-member. Any other bound context is
                // unestablished over this role and its denial is hidden.
                let established = call.auth().is_none()
                    && self
                        .connections
                        .get(id)
                        .is_some_and(|conn| conn.establishes(call.context(), role.as_str()));
                // §10.3/§10.5: membership is confirmed for the SCOPE the request
                // addresses, so a holder scoped to one row is not admitted to another.
                // §10.4: for a caller without established authority every authorize
                // denial over this (unenumerable) role — the whole authentication-
                // FAILURE path (unaccepted authenticator, forged credential, invalid
                // session/actor) as well as non-membership — collapses to the uniform
                // `unresolved`, so it cannot tell an existing role from a nonexistent
                // one by the wire code; an established caller reads the precise reason.
                let context = self
                    .authorize_role(role_def, &selection, call.scope(), &reader)
                    .map_err(|denial| {
                        SurfaceOutcome::Denied(Self::hide_unenumerable_denial(
                            call.address().authority(),
                            established,
                            denial,
                        ))
                    })?;
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

    /// Collapse a pre-authority denial over a target the caller cannot enumerate to
    /// the uniform unresolvable-name denial (§10.4).
    ///
    /// A role catalog is unenumerable. Every denial reason other than the uniform
    /// [`DenialReason::Unresolved`] fires ONLY after a role's existence is confirmed:
    /// a nonexistent role short-circuits to `unresolved` at the `router.role()`
    /// lookup, while an existing role advances to the authenticator-acceptance,
    /// credential-`$verify`, `$check`/`$session`/`$actor`, and membership checks. So
    /// for an [`Authority::Role`] target ANY distinct reason — `unauthenticated` (no
    /// actor), `authenticator-not-accepted`/`-missing`, `unverified` (a forged
    /// credential), `check-failed`, `session-invalid`, `actor-unresolved` — is a
    /// role-existence oracle: `member.x` (exists) denies it while `ghost.x` (absent)
    /// denies `unresolved`.
    ///
    /// §10.4's exception is narrow and TARGET-scoped: "membership- or
    /// existence-specific diagnostics are permitted only toward a caller that has
    /// already established authority over THE TARGET." `established` carries that
    /// predicate, scoped to the target role. It is set only when the request
    /// authorizes from a bound connection context whose recorded role — the role it
    /// authenticated against through [`SurfaceHost::authenticate`] — IS this target
    /// role. `authenticate` denies a nonexistent role `unresolved` and binds
    /// nothing, so a context bound against role R proves the caller already learned R
    /// exists; such a caller reads R's precise reason (e.g. a `session-invalid` once
    /// its session expires or is revoked, §11.7) — hiding R's existence from someone
    /// who already established authority over R is pointless.
    ///
    /// Crucially, a bound context is established authority over its OWN role alone.
    /// A caller with a context bound against role `alpha` probing a different role
    /// `beta` (which it never authenticated to, and which need not even accept
    /// `alpha`'s authenticator) is UNestablished over `beta`: `beta`'s pre-authority
    /// reason (e.g. `authenticator-not-accepted`) is hidden exactly as a fresh
    /// probe's is. Reading the recorded role from a live session is unnecessary — and
    /// would be impossible once the session expires — so the target-scoping rests on
    /// the role captured at `authenticate`, not on re-resolving the actor.
    ///
    /// A caller WITHOUT established authority over the target — a fresh per-request
    /// `auth` probe, no actor at all, or a bound context established against a
    /// *different* role — is `established = false`: every pre-authority reason over a
    /// role target is remapped to the uniform unresolvable-name outcome, identical in
    /// class, code, and message to a nonexistent name ([`Self::unresolved_name`]), so
    /// the whole authentication-FAILURE path (a forged credential, an unaccepted
    /// authenticator, an invalid session/actor — none of which established authority)
    /// cannot betray that the role exists. A non-member's membership failure already
    /// denies the uniform `unresolved` in [`Self::authorize_role`], so it passes
    /// through unchanged whether or not the caller is established.
    ///
    /// An [`Authority::Public`] target is enumerable via `manifest`, so a denial over
    /// it discloses nothing and every reason is preserved. (A public surface can
    /// never in fact require an actor — a `$actor`/`$session` read inside a public
    /// program is rejected at load, and an indirect one faults at admission as a
    /// `rejected`, never a `denied`, §10.2 — so the preserved branch guards a
    /// structural invariant; the remap stays predicated on authority so a caller who
    /// *may* enumerate the target still reads the precise reason.)
    fn hide_unenumerable_denial(authority: &Authority, established: bool, denial: Denial) -> Denial {
        let leaks_existence = matches!(authority, Authority::Role(_))
            && !established
            && denial.reason() != DenialReason::Unresolved;
        if leaks_existence {
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

    /// Close a call's argument object against the resolved mutation's declared
    /// parameters (§12.1): the receiver key names plus the mutation parameters. A
    /// member the mutation does not declare — including any reserved `$`-prefixed
    /// name, which is never a declared parameter — makes the request malformed, so
    /// it is rejected (`Err`) rather than silently dropped and admitted on a
    /// best-effort binding. There is no width subtyping over the external argument
    /// object.
    ///
    /// This is the `call` mirror of [`closed_view_args`](Self::closed_view_args),
    /// and it is what keeps the §12.3 dedup identity exactly the decoded declared
    /// argument set: [`build_request`](Self::build_request) records the model from
    /// the verbatim `args`, so an undeclared member reaching it would silently vary
    /// the dedup identity ("no ignored-but-present member can silently vary");
    /// rejecting here first makes that impossible.
    ///
    /// It runs only after [`resolve_call`](Self::resolve_call) has confirmed the
    /// caller's membership, so surfacing `malformed` — rather than collapsing to the
    /// uniform `Unresolved`/`Denied` — reveals nothing an unauthorized caller could
    /// exploit (§10.4, the SPEC-ISSUES item-8 oracle). Like the view mirror it
    /// closes only where the binding declares a non-empty parameter contract: a
    /// mutation the router reconstructs with no declared parameter (e.g. a
    /// `reinsert(@extract)` erasure mutation whose `@extract` the model does not
    /// surface) has no reliable shape to close against, so it is left unchecked
    /// rather than over-rejecting a legitimate argument — matching the testkit
    /// adapter's closed-shape guard (`call`, non-empty `call_param_names`).
    ///
    /// §18.7: a registered blob-field name is a host-resolved declared parameter —
    /// [`call_with_blob`](Self::call_with_blob) verifies the streamed bytes and
    /// binds the descriptor to it — so it is admitted alongside the binding's own
    /// receiver/params even when the router binding lists only the scalar params.
    /// It is a host concept, never a free-form client member.
    fn closed_call_args(&self, binding: &CallBinding, args: &BTreeMap<String, Value>) -> Result<(), Rejection> {
        let declared: BTreeSet<&str> =
            binding.receiver().iter().chain(binding.params()).map(String::as_str).collect();
        if declared.is_empty() {
            return Ok(());
        }
        match args
            .keys()
            .find(|name| !declared.contains(name.as_str()) && !self.blobs.contains_key(name.as_str()))
        {
            Some(member) => Err(Rejection::new(
                RejectionReason::Malformed,
                format!("argument `{member}` is not a declared parameter of this mutation (§12.1)"),
            )),
            None => Ok(()),
        }
    }

    /// Build the runtime [`CallRequest`] (bound receiver + parameters) and the
    /// §12.3 request model (the declared arguments, closed by
    /// [`closed_call_args`](Self::closed_call_args) before this runs) for dedup
    /// equivalence.
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
        // §12.1: the subscription's argument object is closed against the resolved
        // view's declared `$params`, applied here — after membership is confirmed,
        // before any row flows — exactly as the `call` path closes its arguments.
        if let Err(denial) = self.closed_view_args(&view_name, watch.args()) {
            return Ok(Subscription::Denied(denial));
        }
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
        // §12.1/§12.3: a resume reconstructs the `view` operation, so its argument
        // object is closed the same way a fresh `watch` closes it — an undeclared or
        // reserved `$`-prefixed member is malformed and refused before the stream is
        // rebuilt, keeping the §12.3 dedup identity the decoded declared argument set.
        if let Err(denial) = self.closed_view_args(&view_name, resume.args()) {
            return Ok(Subscription::Denied(denial));
        }
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

    /// Close a subscription's argument object against the resolved view's declared
    /// `$params` (§12.1): the arguments MUST contain only declared parameter names.
    /// A member the view does not declare — including any reserved `$`-prefixed
    /// name, which is never a declared `$params` name — makes the request malformed,
    /// so it is refused (`Err`) rather than silently dropped and served on a
    /// filtered view. There is no width subtyping over the external argument object,
    /// and closing it here keeps the §12.3 dedup identity exactly the decoded
    /// declared argument set ("no ignored-but-present member can silently vary").
    ///
    /// This mirrors the `call` path's closed-shape check
    /// ([`closed_call_args`](Self::closed_call_args) rejects a call the same way). It
    /// runs only after [`resolve_view`](Self::resolve_view)
    /// has confirmed the caller's membership, so surfacing `malformed` — rather than
    /// collapsing to the uniform `Unresolved` — reveals nothing an unauthorized
    /// caller could exploit (§10.4, the `view` mirror of the item-8 `call` oracle).
    ///
    /// The shape is closed only where the view declares a `$params` contract to
    /// close against. A view the model reports as taking NO parameter has no
    /// reliable declared shape here, and its address doubles as the blob `fetch`
    /// descriptor-resolution read (§18.8) — whose row-selector arguments are not
    /// view `$params` — so a paramless view is left unchecked rather than
    /// over-rejecting a legitimate fetch. This matches the testkit adapter's wave-3
    /// closed-shape guard (`open_watch`, `!arg_types.is_empty()`), keeping the real
    /// surface and the corpus-driving adapter consistent.
    ///
    /// [`build_request`]: Self::build_request
    /// [`resolve_view`]: Self::resolve_view
    fn closed_view_args(&self, view_name: &str, args: &BTreeMap<String, Value>) -> Result<(), Denial> {
        let declared: BTreeSet<String> =
            self.engine.surface_view_params(view_name).into_iter().map(|(name, _)| name).collect();
        if declared.is_empty() {
            return Ok(());
        }
        match args.keys().find(|name| !declared.contains(name.as_str())) {
            Some(member) => Err(Denial::new(
                DenialReason::Malformed,
                format!("argument `{member}` is not a declared parameter of this view (§12.1)"),
            )),
            None => Ok(()),
        }
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
    /// one is supplied, otherwise from the connection's stored context. A public
    /// subscription carries NO selection: one attached is malformed and refused
    /// before the credential is inspected (§10.2/§11.4), mirroring the `call` path.
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
                // §10.2/§11.4: a public address carries no authenticator selection. A
                // public subscription that nonetheless attaches one is malformed —
                // refused here, BEFORE the credential is inspected, never dropped and
                // served actor-less (mirrors `resolve_call`'s public arm). The `call`
                // pipeline reports this as a `rejected` admission refusal; a
                // `Subscription` has no `rejected` arm and adding one would ripple into
                // `liasse-connect`/clients, so the refusal surfaces on the shared
                // `denied` channel — still fail-closed, never serving the request. A
                // public target is enumerable via `manifest`, so a reason distinct from
                // a nonexistent public address discloses nothing (§10.4).
                if selection.is_some() {
                    return Err(Denial::new(
                        DenialReason::AuthenticatorNotAccepted,
                        "a public address carries no authenticator selection",
                    ));
                }
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
                                false,
                                Denial::new(DenialReason::Unauthenticated, "the connection is not open"),
                            ));
                        };
                        match connection.select_context(context).cloned() {
                            Some(selection) => selection,
                            None => {
                                return Err(Self::hide_unenumerable_denial(
                                    address.authority(),
                                    false,
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
                // §10.4: as on the `call` path, for a caller without established
                // authority every authorize denial over this (unenumerable) role — the
                // authentication-FAILURE path (unaccepted authenticator, forged
                // credential, invalid session/actor) as well as non-membership —
                // collapses to the uniform `unresolved`, so it cannot enumerate the
                // role by wire code. §10.4's exception is TARGET-scoped: a per-request
                // `auth` selection (`inline`) is a fresh probe (never established), and
                // a bound connection context is established authority over a role only
                // when it authenticated against that role AND was a verified MEMBER of
                // it at authenticate — so it is established here only for this target
                // role and only for a member, never over an unrelated role nor for a
                // non-member whose session merely resolved.
                let established = inline.is_none()
                    && self
                        .connections
                        .get(id)
                        .is_some_and(|conn| conn.establishes(context, role.as_str()));
                let auth_context = self
                    .authorize_role(role_def, &selection, scope, &reader)
                    .map_err(|denial| {
                        Self::hide_unenumerable_denial(address.authority(), established, denial)
                    })?;
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
