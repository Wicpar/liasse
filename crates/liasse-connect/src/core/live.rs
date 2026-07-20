//! The subscription and call pipelines (§12.1–§12.3): opening a live view,
//! reconstructing coherent §12.2 patches after a commit (D6), and a snapshot read.
//!
//! A `call` runs the host's §12.1 pipeline, then — before returning `committed` —
//! reconciles every still-authorized subscription on the same connection: it
//! re-projects the recomputed authorized view to wire rows and diffs them against
//! the client's retained wire snapshot ([`frames::diff_rows`]), enqueuing the
//! resulting patch on the SSE stream. Patches are therefore enqueued *before* the
//! committed reply returns (§12.3), by construction. A peer connection's commit is an
//! outgoing frontier that can revoke authority, so a lost-authority subscription on a
//! peer is closed here too — but its rows are not advanced (§12.3).

use std::collections::BTreeMap;

use liasse_store::InstanceStore;
use liasse_surface::{
    Authority, CommitSeq, KeyProvider, OperationKey, SurfaceAddress, SurfaceCall, SurfaceOutcome,
    SurfaceWatch, Subscription, ViewResult, ViewRow, Window,
};
use liasse_wire::serde_json::Value as Json;
use liasse_wire::{ConnectionToken, Downstream, Ft, OperationId, Sub, WireWindow};
use liasse_wire::{WireAnchor, serde_json::Value as WireValue};

use crate::decode;
use crate::encode;
use crate::error::ConnectError;

use super::frames::project_rows;
use super::registry::{AnchorResolution, SubState};
use super::{ConnectCore, Reply};

/// How a bounded window request resolved against the connection's occurrence index.
enum WindowBuild {
    /// A window ready to open.
    Ready(Window),
    /// A well-formed anchor for an occurrence the connection does not hold (§12.2).
    Absent,
    /// A forged anchor token — a transport fault.
    Forged,
}

impl<S: InstanceStore, P: KeyProvider> ConnectCore<S, P> {
    /// Open (or replace) a live subscription over a surface view (§12.1 `view`,
    /// §12.2). The initial `init`/`scalar` is enqueued on the SSE stream; the reply
    /// only reports the opening frontier, or a refusal.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn view(
        &mut self,
        token: &ConnectionToken,
        sub: Sub,
        address: &str,
        params: Option<&Json>,
        window: Option<WireWindow>,
        auth: Option<&Json>,
        context: Option<&Json>,
    ) -> Result<Reply, ConnectError> {
        let Ok(addr) = SurfaceAddress::parse(address) else {
            return Ok(Reply::Outcome(encode::unresolved()));
        };
        let context_name = match decode::decode_context(context) {
            Ok(name) => name,
            Err(error) => return Ok(Reply::Outcome(encode::decode_error(&error))),
        };
        // §11.4: decode the authenticator selection up front — it gates authorization,
        // which must settle before the closed-shape `$params` decode is revealed. A
        // native-cose credential is gated through the engine's cose verify here
        // ([`ConnectCore::decode_selection`]) so the surface verifier receives the
        // verified claims, never the raw token (§17.7).
        let selection = match auth {
            Some(auth) => match self.decode_selection(auth) {
                Ok(selection) => Some(selection),
                Err(_) => return Ok(Reply::Outcome(encode::unverified())),
            },
            None => None,
        };

        // §10.4/§12.1 (GitHub #39): settle the denied/allowed disposition from name
        // resolution and membership BEFORE the closed-shape `$params` decode, so a
        // non-member's refusal is a uniform `denied` independent of the params shape or
        // whether the view exists. The authorization probe carries no params —
        // resolution and membership never read them — so an existing ungranted role
        // view and a nonexistent one deny identically, and the decode's
        // `malformed`/param-type reveal (item 6/#10) is withheld from a caller that has
        // not established authority over the target.
        let mut probe = SurfaceWatch::new(addr.clone(), sub.as_str());
        if let Some(name) = &context_name {
            probe = probe.with_context(name.clone());
        }
        if let Some(selection) = &selection {
            probe = probe.with_auth(selection.clone());
        }
        if let Err(denial) = self.host.authorize_view(token.as_str(), &probe) {
            return Ok(Reply::Outcome(encode::denied(&denial)));
        }

        // Authorized: the closed-shape `$params` decode may now reveal a malformed or
        // unknown parameter to this authorized caller (§10.1, item 6/#10).
        let args = match decode::decode_args(self.schema.view_params(address), params) {
            Ok(args) => args,
            Err(error) => return Ok(Reply::Outcome(encode::decode_error(&error))),
        };
        let mut watch = SurfaceWatch::new(addr, sub.as_str()).with_args(args);
        if let Some(name) = &context_name {
            watch = watch.with_context(name.clone());
        }
        if let Some(selection) = selection {
            watch = watch.with_auth(selection);
        }
        if let Some(wire) = window {
            match self.build_window(token, &wire) {
                WindowBuild::Ready(win) => watch = watch.with_window(win),
                WindowBuild::Absent => return Ok(Reply::Outcome(encode::absent_anchor())),
                WindowBuild::Forged => return Err(ConnectError::BadToken),
            }
        }

        match self.host.watch(token.as_str(), &watch)? {
            Subscription::Init(result) => {
                let ft = self.open_subscription(token, &sub, &result, false)?;
                Ok(Reply::Opened { frontier: ft })
            }
            Subscription::Window(rows) => {
                let ft = self.install_rows(token, &sub, rows, true)?;
                Ok(Reply::Opened { frontier: ft })
            }
            Subscription::Denied(denial) => Ok(Reply::Outcome(encode::denied(&denial))),
            Subscription::Failed(error) => Ok(Reply::Outcome(encode::window_failure(&error))),
        }
    }

    /// Invoke a surface call (§12.1), reconcile the caller's subscriptions through the
    /// commit (§12.3 D6), and report the outcome. §12.3 patches are enqueued before
    /// this returns.
    pub(super) fn call(
        &mut self,
        token: &ConnectionToken,
        operation: Option<OperationId>,
        address: &str,
        args: &Json,
        auth: Option<&Json>,
        context: Option<&Json>,
    ) -> Result<Reply, ConnectError> {
        let Ok(addr) = SurfaceAddress::parse(address) else {
            return Ok(Reply::Outcome(encode::unresolved()));
        };
        let context_name = match decode::decode_context(context) {
            Ok(name) => name,
            Err(error) => return Ok(Reply::Outcome(encode::decode_error(&error))),
        };
        let selection = match auth {
            Some(auth) => match self.decode_selection(auth) {
                Ok(selection) => Some(selection),
                Err(_) => return Ok(Reply::Outcome(encode::unverified())),
            },
            None => None,
        };

        // §10.4/§12.1 (SPEC-ISSUES item 8): settle the denied/allowed disposition
        // from name resolution and membership BEFORE the closed-shape argument
        // decode, so a non-member's refusal is a uniform `denied` independent of the
        // argument payload. The authorization probe carries no arguments —
        // resolution and membership never read them — so an existing ungranted call
        // and a nonexistent one deny identically whatever the probe's shape, and the
        // decode's `malformed`/arg-type reveal (item 6) is withheld from a caller
        // that has not established authority over the target.
        let mut probe = SurfaceCall::new(addr.clone(), BTreeMap::new());
        if let Some(name) = &context_name {
            probe = probe.with_context(name.clone());
        }
        if let Some(selection) = &selection {
            probe = probe.with_auth(selection.clone());
        }
        if let Err(refusal) = self.host.authorize_call(token.as_str(), &probe) {
            return self.outcome_reply(token, &refusal);
        }

        // Authorized: the closed-shape decode may now reveal a malformed argument or
        // an unknown member to this authorized caller (SPEC-ISSUES item 6).
        let decoded = match decode::decode_args(self.schema.call_args(address), Some(args)) {
            Ok(args) => args,
            Err(error) => return Ok(Reply::Outcome(encode::decode_error(&error))),
        };
        // §12.3 / BUG 2: a PUBLIC operation id is bound to this connection's secret
        // before it reaches the host's dedup log, so two anonymous connections never
        // share one op-id namespace — a peer can neither replay this connection's
        // retained response nor burn its id. A ROLE id keeps its SPEC authenticator
        // scope. The bound value is server-internal; the client still echoes the raw
        // id (its §12.3 status-query capability).
        let scoped = operation.as_ref().map(|id| self.scope_operation(token, &addr, id));
        let mut call = SurfaceCall::new(addr.clone(), decoded);
        if let Some(scoped) = &scoped {
            call = call.with_operation_id(scoped);
        }
        if let Some(name) = &context_name {
            call = call.with_context(name.clone());
        }
        if let Some(selection) = selection {
            call = call.with_auth(selection);
        }

        let outcome = self.host.call(token.as_str(), &call)?;
        if let Some(id) = operation
            && !matches!(outcome, SurfaceOutcome::Denied(_))
        {
            // Record under the RAW id the client will present at status time, keyed by
            // the SCOPED id the host actually deduplicated on.
            let effective = scoped.as_deref().unwrap_or_else(|| id.as_str());
            let key = self.operation_key(token, &addr, &call, effective);
            if let Some(state) = self.connections.get_mut(token) {
                state.record_operation(id, key);
            }
        }
        if let Some(commit) = outcome.commit() {
            self.reconcile_after_commit(token, commit);
        }
        self.outcome_reply(token, &outcome)
    }

    /// Encode a settled (or refused) surface outcome onto the wire for `token`,
    /// minting any frontier/commit position through the connection's key material.
    /// Shared by the authorization-refusal short-circuit and the executed-call path
    /// so both render an outcome identically.
    fn outcome_reply(
        &self,
        token: &ConnectionToken,
        outcome: &SurfaceOutcome,
    ) -> Result<Reply, ConnectError> {
        let minter = self.minter.as_ref();
        let state = self.connections.get(token).ok_or(ConnectError::NoConnection)?;
        let wire =
            encode::outcome_of(outcome, |seq: CommitSeq| state.keys().frontier(minter, seq.get()));
        Ok(Reply::Outcome(wire))
    }

    /// Read a value once at the current frontier (§12.1 `fetch`). Composed as an
    /// authorized transient subscription over a throwaway connection, so it reuses the
    /// host's projection and authorization without editing the surface (D5) and leaves
    /// no residue on the client's connection.
    pub(super) fn fetch(
        &mut self,
        token: &ConnectionToken,
        address: &str,
        params: Option<&Json>,
    ) -> Result<Reply, ConnectError> {
        let Ok(addr) = SurfaceAddress::parse(address) else {
            return Ok(Reply::Outcome(encode::unresolved()));
        };
        let scratch = format!("{}::fetch", token.as_str());
        self.host.connect(&scratch)?;

        // §10.4/§12.1 (GitHub #39): authorize the params-free target BEFORE the
        // closed-shape `$params` decode, so an existing ungranted role view (here,
        // always unauthenticated — the scratch connection carries no actor) and a
        // nonexistent one refuse identically, instead of the existing view decoding
        // its `$params` cleanly while the nonexistent one rejects them as unknown.
        let probe = SurfaceWatch::new(addr.clone(), "fetch");
        if let Err(denial) = self.host.authorize_view(&scratch, &probe) {
            self.host.disconnect(&scratch);
            return Ok(Reply::Outcome(encode::denied(&denial)));
        }

        // Authorized: the closed-shape `$params` decode may now reveal a malformed or
        // unknown parameter to this authorized caller (§10.1, item 6/#10).
        let args = match decode::decode_args(self.schema.view_params(address), params) {
            Ok(args) => args,
            Err(error) => {
                self.host.disconnect(&scratch);
                return Ok(Reply::Outcome(encode::decode_error(&error)));
            }
        };
        let watch = SurfaceWatch::new(addr, "fetch").with_args(args);
        let reply = match self.host.watch(&scratch, &watch) {
            Ok(Subscription::Init(result)) => Reply::Fetched(fetch_value(&result)),
            Ok(Subscription::Window(rows)) => {
                Reply::Fetched(Json::Array(rows.iter().map(encode::row_object).collect()))
            }
            Ok(Subscription::Denied(denial)) => Reply::Outcome(encode::denied(&denial)),
            Ok(Subscription::Failed(error)) => Reply::Outcome(encode::window_failure(&error)),
            Err(error) => {
                self.host.disconnect(&scratch);
                return Err(error.into());
            }
        };
        self.host.disconnect(&scratch);
        Ok(reply)
    }

    /// Install a freshly opened unwindowed subscription and emit its `init`/`scalar`.
    fn open_subscription(
        &mut self,
        token: &ConnectionToken,
        sub: &Sub,
        result: &ViewResult,
        windowed: bool,
    ) -> Result<Ft, ConnectError> {
        match result.scalar() {
            Some(value) => self.install_scalar(token, sub, value.to_wire()),
            None => self.install_rows(token, sub, result.rows().to_vec(), windowed),
        }
    }

    /// Install a row-stream subscription, project its rows, and enqueue the `init`.
    fn install_rows(
        &mut self,
        token: &ConnectionToken,
        sub: &Sub,
        rows: Vec<ViewRow>,
        windowed: bool,
    ) -> Result<Ft, ConnectError> {
        let seq = self.frontier_seq(token);
        let minter = self.minter.as_ref();
        let state = self.connections.get_mut(token).ok_or(ConnectError::NoConnection)?;
        state.insert_sub(sub.clone(), SubState::rows(windowed));
        let snapshot = project_rows(state, minter, sub, &rows);
        if let Some(sub_state) = state.sub_mut(sub) {
            sub_state.snapshot = snapshot.clone();
        }
        let ft = state.keys().frontier(minter, seq);
        state.outbound_mut().enqueue(ft.clone(), seq, Downstream::Init { sub: sub.clone(), rows: snapshot });
        Ok(ft)
    }

    /// Install a scalar subscription and enqueue its `scalar` value (§7.5).
    fn install_scalar(
        &mut self,
        token: &ConnectionToken,
        sub: &Sub,
        value: WireValue,
    ) -> Result<Ft, ConnectError> {
        let seq = self.frontier_seq(token);
        let minter = self.minter.as_ref();
        let state = self.connections.get_mut(token).ok_or(ConnectError::NoConnection)?;
        state.insert_sub(sub.clone(), SubState::scalar());
        if let Some(sub_state) = state.sub_mut(sub) {
            sub_state.scalar_value = Some(value.clone());
        }
        let ft = state.keys().frontier(minter, seq);
        state.outbound_mut().enqueue(ft.clone(), seq, Downstream::Scalar { sub: sub.clone(), value });
        Ok(ft)
    }

    /// Build a bounded window from its wire request, resolving a concrete anchor
    /// against this connection's occurrence index (§12.2).
    fn build_window(&self, token: &ConnectionToken, wire: &WireWindow) -> WindowBuild {
        match &wire.anchor {
            WireAnchor::First => WindowBuild::Ready(Window::first(wire.size)),
            WireAnchor::Last => WindowBuild::Ready(Window::last(wire.size)),
            WireAnchor::At { occ } => {
                let Some(state) = self.connections.get(token) else {
                    return WindowBuild::Forged;
                };
                match state.resolve_occ(self.minter.as_ref(), occ) {
                    AnchorResolution::Row(row) => {
                        let mut window = Window::anchored(wire.size, row);
                        if wire.slide {
                            window = window.sliding();
                        }
                        WindowBuild::Ready(window)
                    }
                    AnchorResolution::Absent => WindowBuild::Absent,
                    AnchorResolution::Forged => WindowBuild::Forged,
                }
            }
        }
    }

    /// The effective operation identifier handed to the host's §12.3 dedup log. A
    /// PUBLIC id is bound to this connection's secret (BUG 2) so a peer connection
    /// cannot forge, replay, or burn it; a ROLE id keeps its SPEC scope
    /// (application + target + authenticator + id, §12.3/§D.8) unchanged, so the same
    /// actor's at-most-once retry still deduplicates. Missing connection state (the
    /// call would already fault at the host) falls back to the raw id, never panics.
    fn scope_operation(&self, token: &ConnectionToken, addr: &SurfaceAddress, id: &OperationId) -> String {
        match addr.authority() {
            Authority::Public => self
                .connections
                .get(token)
                .map_or_else(|| id.as_str().to_owned(), |state| state.keys().scope_operation(id.as_str())),
            Authority::Role(_) => id.as_str().to_owned(),
        }
    }

    /// Reconstruct the §12.3 operation scope key a later status query reads. It MUST
    /// equal the key the host deduplicated on, so `op_id` is the already-scoped
    /// effective identifier. Public calls introduce no actor (no authenticator); a
    /// role call keys on its selection's authenticator, falling back to the
    /// connection context's.
    fn operation_key(
        &self,
        token: &ConnectionToken,
        addr: &SurfaceAddress,
        call: &SurfaceCall,
        op_id: &str,
    ) -> OperationKey {
        let auth = match addr.authority() {
            Authority::Public => None,
            Authority::Role(_) => call.auth().map(|s| s.auth().to_owned()).or_else(|| {
                let context = call.context().unwrap_or("default");
                self.connections
                    .get(token)
                    .and_then(|state| state.bound_context_auth(context))
                    .map(str::to_owned)
            }),
        };
        OperationKey::new(addr.surface_prefix(), auth, op_id)
    }
}

/// The projected value of a snapshot read (§12.1 `fetch`): an array of row objects,
/// or a scalar rendered directly.
fn fetch_value(result: &ViewResult) -> Json {
    match result.scalar() {
        Some(value) => value.to_wire(),
        None => Json::Array(result.rows().iter().map(encode::row_object).collect()),
    }
}
