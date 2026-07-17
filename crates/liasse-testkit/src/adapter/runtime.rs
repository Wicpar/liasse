//! One runtime instance: the surface host over a store plus the volatile client
//! state a scenario drives against it.
//!
//! The scenario adapter runs one *base* instance over the case's store, and — for
//! a §19.10 restore inside an `in_sandbox` group — one or more *sandbox* instances
//! over a throwaway in-memory store. Both are a [`Runtime`]; the only difference is
//! the store type, so every client verb (`connect`/`call`/`watch`/…) and every §19
//! host operation (`export`/`import`/`reconcile`/`classify`) is written once,
//! generic over the store, and reached through the object-safe [`Instance`] trait
//! so the adapter can drive whichever instance is active without caring which
//! backend sits underneath. Byte-level artifact passing and sandbox lifecycle stay
//! in the adapter ([`super::ScenarioAdapter`]); a [`Runtime`] only ever touches its
//! own host.

use std::collections::{BTreeMap, BTreeSet};

use liasse_runtime::{CommitSeq, ImportError, ImportRelation, Precision, Timestamp};
use liasse_store::InstanceStore;
use liasse_surface::{
    AuthResult, OperationKey, OperationStatus, SurfaceAddress, SurfaceCall, SurfaceHost,
    SurfaceResume, SurfaceWatch, VirtualClock as SurfaceClock,
};

use crate::clock::VirtualClock;
use crate::contract::{CallRequest, ConnectRequest, Observation, WatchRequest};
use crate::id::{ConnectionId, WatchId};
use crate::outcome::{Completion, Outcome};

use super::auth::AuthPlan;
use super::lift::SurfaceLift;
use super::router::Routing;
use super::{
    build_window, connection_name, host_fault, observe_call, observe_subscription, parse_auth_selection,
    wire, AdapterError, Loaded, State,
};

/// One live runtime instance: the loaded surface host (or the reason it did not
/// load) plus the volatile per-connection state a scenario builds up against it.
pub(super) struct Runtime<S: InstanceStore> {
    pub(super) state: State<S>,
    /// Which connection each open subscription lives on.
    pub(super) watch_conns: BTreeMap<String, String>,
    /// Which open subscriptions watch a singular view (§12.2).
    pub(super) watch_singular: BTreeMap<String, bool>,
    /// The full request each open subscription opened with, so a host rebuild that
    /// reaches `&mut Engine` (the §17.9 `provider_set` fault path) can re-establish
    /// every live subscription over the rebuilt host rather than lose it.
    pub(super) watch_specs: BTreeMap<String, WatchRequest>,
    /// The connection ids currently open on the host.
    pub(super) open_connections: BTreeSet<String>,
    /// The §12.3 operation key each submitted `operation_id` scoped to, so a later
    /// `operation_status` step reconstructs the exact key the call recorded under
    /// (the identifier is the capability; an unknown identifier maps to no key).
    pub(super) op_keys: BTreeMap<String, OperationKey>,
    /// The last committed §18 blob digest per blob field, so a `blob_get` fetches
    /// by digest and a `connector_set { corrupt }` targets the blob under test.
    pub(super) blob_digests: BTreeMap<String, String>,
    /// The adapter-side virtual clock, used to compute the absolute instant an
    /// `advance_time` moves this instance's surface clock to.
    pub(super) clock: VirtualClock,
}

impl<S: InstanceStore> Runtime<S> {
    /// Wrap a freshly loaded stack as a base runtime with no open connections.
    pub(super) fn new(state: State<S>) -> Self {
        Self {
            state,
            watch_conns: BTreeMap::new(),
            watch_singular: BTreeMap::new(),
            watch_specs: BTreeMap::new(),
            open_connections: BTreeSet::new(),
            op_keys: BTreeMap::new(),
            blob_digests: BTreeMap::new(),
            clock: VirtualClock::new(),
        }
    }

    /// The loaded stack, or the load failure recorded in its place.
    pub(super) fn loaded(&mut self) -> Result<&mut Loaded<S>, AdapterError> {
        match &mut self.state {
            State::Loaded(loaded) => Ok(&mut **loaded),
            State::Failed(message) => Err(AdapterError::LoadFailed(message.clone())),
        }
    }

    /// Take the loaded stack out, leaving a sentinel failure — used to move the
    /// owned host through [`SurfaceHost::into_parts`]. The caller must restore a
    /// `Loaded` state before returning.
    fn take_loaded(&mut self) -> Result<Loaded<S>, AdapterError> {
        let taken = std::mem::replace(&mut self.state, State::Failed("stack in transit".to_owned()));
        match taken {
            State::Loaded(loaded) => Ok(*loaded),
            State::Failed(message) => {
                self.state = State::Failed(message.clone());
                Err(AdapterError::LoadFailed(message))
            }
        }
    }

    /// Restore the loaded stack after a handoff.
    fn reinstate(&mut self, loaded: Loaded<S>) {
        self.state = State::Loaded(Box::new(loaded));
    }

    /// Ensure `connection` is open on the host before a call/watch runs on it. The
    /// executor resolves a step's connection against its own open set, which spans
    /// the whole run; a freshly activated sandbox instance has not opened that
    /// connection yet, so open it lazily at the head frontier on first use.
    fn ensure_connection(&mut self, connection: &str) {
        if !self.open_connections.insert(connection.to_owned()) {
            return;
        }
        if let State::Loaded(loaded) = &mut self.state {
            loaded.host.connect(connection.to_owned());
        }
    }

    /// Replace this instance's loaded stack, dropping any prior connections — used
    /// when a §19.10 `restore` activates a sandbox slot.
    pub(super) fn install(&mut self, loaded: Loaded<S>) {
        self.state = State::Loaded(Box::new(loaded));
        self.open_connections.clear();
        self.watch_conns.clear();
        self.watch_singular.clear();
        self.watch_specs.clear();
        self.op_keys.clear();
        self.blob_digests.clear();
    }

    /// Re-open every tracked connection on the current host — a §9.2 lifecycle
    /// load rebuilds the host but does not drop clients.
    fn reopen_connections(&mut self) {
        let ids: Vec<String> = self.open_connections.iter().cloned().collect();
        if let State::Loaded(loaded) = &mut self.state {
            for id in ids {
                loaded.host.connect(id);
            }
        }
    }

    /// Open subscription `request` on its connection over the current host,
    /// recording which connection it lives on and whether it is singular, and
    /// return the observed init. Shared by the first `watch` and a rebuild replay.
    fn open_watch(&mut self, request: &WatchRequest) -> Result<Observation, AdapterError> {
        let connection = connection_name(request.on.as_ref());
        self.ensure_connection(&connection);
        let watch_id = request.id.to_string();
        let (observation, singular) = {
            let loaded = self.loaded()?;
            let address = SurfaceAddress::parse(&request.target)
                .map_err(|err| AdapterError::Host(format!("malformed address `{}`: {err}", request.target)))?;
            let mut watch = SurfaceWatch::new(address, watch_id.clone());
            // §10.1/§12.1: a parameterized `$view` reads its `$params` from the
            // subscription's arguments, decoded against the view's declared types.
            let arg_types = loaded.routing.view_arg_types(&request.target);
            let args = wire::decode_args(&request.args, &arg_types);
            if !args.is_empty() {
                watch = watch.with_args(args);
            }
            if let Some(window) = request.window.as_ref().and_then(build_window) {
                watch = watch.with_window(window);
            }
            // §11.4: a subscription may carry its own authenticator selection,
            // authorizing inline rather than reusing the connection's context — the
            // §11.8 multiplex path where one connection carries several sessions.
            if let Some(selection) = request.auth.as_ref().and_then(super::parse_auth_selection) {
                watch = watch.with_auth(selection);
            }
            // §11.8: on a multiplexed connection the subscription names which
            // authenticated context it runs under, so it applies that context's
            // authorization and projection independently of any other context.
            if let Some(context) = &request.context {
                watch = watch.with_context(context.clone());
            }
            // §12.2: a singular view delivers one object; a collection a row array.
            let singular = loaded.routing.is_singular_view(&request.target);
            let subscription = loaded.host.watch(&connection, &watch).map_err(host_fault)?;
            (observe_subscription(&subscription, singular), singular)
        };
        self.watch_conns.insert(watch_id.clone(), connection);
        self.watch_singular.insert(watch_id, singular);
        Ok(observation)
    }

    /// Re-establish every retained subscription over the current host — after a
    /// rebuild that reached `&mut Engine`, the rebuilt host carries no
    /// subscriptions. Each is re-opened at its connection's current frontier; a
    /// subscription that no longer authorizes is simply not re-established.
    fn replay_watches(&mut self) {
        let specs: Vec<WatchRequest> = self.watch_specs.values().cloned().collect();
        for spec in specs {
            let _ = self.open_watch(&spec);
        }
    }

    /// Rebuild the host over its engine after applying `mutate` to the engine — the
    /// only way to reach `&mut Engine` while keeping the durable store and clock,
    /// used by the §17.9 `provider_set` fault path (the engine keyring's provider
    /// is reconfigured here). Committed state, connections, and live subscriptions
    /// are preserved: the store and clock are retained across
    /// [`SurfaceHost::into_parts`], and connections and subscriptions are
    /// re-established over the rebuilt host.
    fn rebuild_engine<T>(
        &mut self,
        mutate: impl FnOnce(&mut liasse_runtime::Engine<S>) -> T,
    ) -> Result<T, AdapterError> {
        let loaded = self.take_loaded()?;
        let routing = loaded.routing;
        let blobs = loaded.blobs;
        let (mut engine, router, clock) = loaded.host.into_parts();
        let result = mutate(&mut engine);
        self.reinstate(Loaded { host: SurfaceHost::new(engine, router, clock), routing, blobs });
        self.reopen_connections();
        self.replay_watches();
        Ok(result)
    }
}

/// A store-erased view of a runtime instance the adapter drives, so the base
/// instance (over the case's store) and a sandbox instance (over a throwaway
/// in-memory store) present the identical surface. Every method mirrors one
/// [`Driver`](crate::Driver) verb or one §19 host operation.
pub(super) trait Instance {
    fn connect(&mut self, request: ConnectRequest) -> Result<Observation, AdapterError>;
    fn disconnect(&mut self, connection: &ConnectionId) -> Result<Observation, AdapterError>;
    fn call(&mut self, request: CallRequest) -> Result<Observation, AdapterError>;
    fn watch(&mut self, request: WatchRequest) -> Result<Observation, AdapterError>;
    fn unwatch(&mut self, id: &WatchId) -> Result<Observation, AdapterError>;
    fn read_view(&mut self, id: &WatchId) -> Result<Observation, AdapterError>;
    /// The close reason of subscription `watch_id` (§12.2): an `ok` observation
    /// whose value is the reason string when the subscription has closed, or a
    /// value-less `ok` when it is still live.
    fn close_reason(&mut self, watch_id: &str) -> Result<Observation, AdapterError>;
    fn advance_time(&mut self, duration: &crate::clock::Iso8601Duration) -> Result<Observation, AdapterError>;
    fn restart(&mut self) -> Result<Observation, AdapterError>;
    fn host_load(&mut self, package: &serde_json::Value) -> Result<Observation, AdapterError>;
    fn operator(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError>;
    /// Query the §12.3 retained status of the operation `id` scoped by a prior
    /// call. The identifier is the capability: an id no call recorded maps to no
    /// key, so the runtime reports `unknown` and reveals nothing.
    fn operation_status(&mut self, id: &str) -> Result<Observation, AdapterError>;
    /// The §12.1 `manifest`: the surfaces granted to `connection`'s context.
    fn manifest(&mut self, connection: &str, context: Option<&str>) -> Result<Observation, AdapterError>;
    /// Resume subscription `watch_id` over surface `address` on `connection` from
    /// the retained frontier `from` (§12.2), rendering the reconstructed result.
    fn resume(
        &mut self,
        connection: &str,
        address: &str,
        watch_id: &str,
        from: CommitSeq,
    ) -> Result<Observation, AdapterError>;
    /// Authenticate a context on `connection` (§11.4/§11.8): a `denied` outcome for
    /// a selection the targeted role does not accept, `ok` when a context binds.
    fn authenticate_op(
        &mut self,
        connection: &str,
        payload: &serde_json::Value,
        context: Option<&str>,
    ) -> Result<Observation, AdapterError>;
    /// Export the committed boundary as `.liasse` bytes (§19.5).
    fn export(&mut self) -> Result<Vec<u8>, AdapterError>;
    /// Import `bytes` under `policy` (§19.8), rendering the movement report.
    fn import(&mut self, bytes: &[u8], policy: &[ImportRelation]) -> Result<Observation, AdapterError>;
    /// Compute the §19.9 three-way merge of `base`/`incoming` against local state.
    fn reconcile(
        &mut self,
        base: &[u8],
        incoming: &[u8],
        policy: &[ImportRelation],
    ) -> Result<Observation, AdapterError>;
    /// §18.7 `blob_put`: stage and verify a blob parameter, then admit the
    /// containing mutation over the composed §18 blob host.
    fn blob_put(&mut self, spec: &super::blobs::BlobPutSpec) -> Result<Observation, AdapterError>;
    /// §18.12 `connector_set`: reconfigure a simulated connector from this step
    /// onward (unavailability, per-operation failure, stored-object corruption).
    fn connector_set(&mut self, spec: &super::blobs::ConnectorSetSpec) -> Result<Observation, AdapterError>;
    /// §17.9 `provider_set`: reconfigure the engine keyring's backing provider from
    /// this step onward (total outage, per-operation clean failure, hang, or an
    /// invalid public key), so a later `cose.sign` mutation or due rotation fails.
    fn provider_set(&mut self, spec: &super::keyrings::ProviderSetSpec) -> Result<Observation, AdapterError>;
    /// §17.3/§17.4 `keyring_admin`: a keyring lifecycle transition
    /// (`bind_activate`/`revoke`/`destroy`) against the engine's self-provisioned
    /// ring.
    fn keyring_admin(&mut self, spec: &super::keyrings::KeyringAdminSpec) -> Result<Observation, AdapterError>;
}

impl<S: InstanceStore> Instance for Runtime<S> {
    fn connect(&mut self, request: ConnectRequest) -> Result<Observation, AdapterError> {
        let connection = request.connection.to_string();
        self.open_connections.insert(connection.clone());
        let loaded = self.loaded()?;
        loaded.host.connect(connection.clone());
        // §11.4: bind the authenticated context on the connection so later role
        // calls run under it, and reflect the authentication outcome so a
        // `connect { authenticate }` step asserting `ok`/`denied` observes the real
        // result. A cose credential is gated through `Engine::cose_verify` (§17.7)
        // before the surface authenticator resolves it.
        let Some(payload) = request.authenticate.as_ref() else {
            return Ok(Observation::ok(None));
        };
        let Some(auth) = super::resolve_authenticate(loaded, payload) else {
            // A payload the wiring does not cover binds no context; the denial is
            // the observable outcome (§11.4).
            return Ok(Observation::outcome(Outcome::Denied));
        };
        match loaded.host.authenticate(&connection, &auth).map_err(host_fault)? {
            AuthResult::Bound => Ok(Observation::ok(None)),
            AuthResult::Denied(_) => Ok(Observation::outcome(Outcome::Denied)),
        }
    }

    fn disconnect(&mut self, connection: &ConnectionId) -> Result<Observation, AdapterError> {
        self.open_connections.remove(&connection.to_string());
        let loaded = self.loaded()?;
        loaded.host.disconnect(&connection.to_string());
        Ok(Observation::ok(None))
    }

    fn call(&mut self, request: CallRequest) -> Result<Observation, AdapterError> {
        let connection = connection_name(request.on.as_ref());
        self.ensure_connection(&connection);
        // §11.4: a per-request authenticator selection on the call overrides the
        // connection's stored context for this one request; its authenticator name
        // is part of the §12.3 operation scope, so capture it before the borrow.
        let selection = request.auth.as_ref().and_then(parse_auth_selection);
        let auth_name = selection.as_ref().map(|s| s.auth().to_owned());
        let (observation, op_record) = {
            let loaded = self.loaded()?;
            let address = SurfaceAddress::parse(&request.target)
                .map_err(|err| AdapterError::Host(format!("malformed address `{}`: {err}", request.target)))?;
            // §12.3: the retained operation scopes to the surface target, the selected
            // authenticator, and the identifier — the exact key the host records under.
            let surface_prefix = address.surface_prefix();
            let types = loaded.routing.arg_types(&request.target);
            let args = wire::decode_args(&request.args, &types);
            let mut call = SurfaceCall::new(address, args);
            if let Some(operation_id) = &request.operation_id {
                call = call.with_operation_id(operation_id.clone());
            }
            if let Some(selection) = selection {
                call = call.with_auth(selection);
            }
            // §11.8: on a multiplexed connection the call names which authenticated
            // context it runs under, so the request binds the actor of that context.
            if let Some(context) = &request.context {
                call = call.with_context(context.clone());
            }
            let outcome = loaded.host.call(&connection, &call).map_err(host_fault)?;
            let op_record = request
                .operation_id
                .as_ref()
                .map(|opid| (opid.clone(), OperationKey::new(surface_prefix, auth_name, opid.clone())));
            (observe_call(&outcome), op_record)
        };
        if let Some((opid, key)) = op_record {
            self.op_keys.insert(opid, key);
        }
        Ok(observation)
    }

    fn watch(&mut self, request: WatchRequest) -> Result<Observation, AdapterError> {
        let observation = self.open_watch(&request)?;
        // Retain the opening request so a host rebuild (the §17.9 `provider_set`
        // path) can re-establish this subscription over the rebuilt host.
        self.watch_specs.insert(request.id.to_string(), request);
        Ok(observation)
    }

    fn unwatch(&mut self, id: &WatchId) -> Result<Observation, AdapterError> {
        self.watch_conns.remove(&id.to_string());
        self.watch_singular.remove(&id.to_string());
        self.watch_specs.remove(&id.to_string());
        Ok(Observation::ok(None))
    }

    fn read_view(&mut self, id: &WatchId) -> Result<Observation, AdapterError> {
        let connection = self.watch_conns.get(&id.to_string()).cloned();
        let watch_id = id.to_string();
        let singular = self.watch_singular.get(&watch_id).copied().unwrap_or(false);
        let loaded = self.loaded()?;
        let value = connection.as_deref().and_then(|conn| {
            loaded
                .host
                .read_window(conn, &watch_id)
                .map(wire::rows_to_json)
                .or_else(|| loaded.host.read_view(conn, &watch_id).map(|r| wire::view_to_json_shaped(r, singular)))
        });
        Ok(Observation::ok(value))
    }

    fn close_reason(&mut self, watch_id: &str) -> Result<Observation, AdapterError> {
        let connection = self.watch_conns.get(watch_id).cloned();
        let loaded = self.loaded()?;
        // §12.2: the subscription's close reason, or absent while it is still live.
        let reason = connection
            .as_deref()
            .and_then(|conn| loaded.host.close_reason(conn, watch_id))
            .map(|reason| serde_json::Value::String(reason.to_owned()));
        Ok(Observation::ok(reason))
    }

    fn advance_time(&mut self, duration: &crate::clock::Iso8601Duration) -> Result<Observation, AdapterError> {
        let instant = self.clock.advance(duration);
        let now = Timestamp::new(i128::from(instant.unix_micros()), Precision::Micros);
        let loaded = self.loaded()?;
        // §14.1/§22.6: advancing time re-evaluates every live view at the new
        // instant and moves the session-expiry and bucket clocks.
        loaded.host.advance_time(now).map_err(host_fault)?;
        Ok(Observation::ok(None))
    }

    fn restart(&mut self) -> Result<Observation, AdapterError> {
        // §22 restart/durability: tear the host down and rebuild a fresh one over
        // the same engine, router, and clock. Committed state survives; volatile
        // connections/subscriptions/operation records are dropped.
        let loaded = self.take_loaded()?;
        let routing = loaded.routing;
        let blobs = loaded.blobs;
        let (engine, router, clock) = loaded.host.into_parts();
        self.reinstate(Loaded { host: SurfaceHost::new(engine, router, clock), routing, blobs });
        self.open_connections.clear();
        Ok(Observation::ok(None))
    }

    fn host_load(&mut self, package: &serde_json::Value) -> Result<Observation, AdapterError> {
        let loaded = self.take_loaded()?;
        let old_routing = loaded.routing.clone();
        let blobs = loaded.blobs;
        let (mut engine, old_router, mut clock) = loaded.host.into_parts();
        // §9.2 host lifecycle reload: a `load(target)` step carries no `hosts`
        // block, so its verifier tables are unchanged from the base load. A
        // host-namespace authenticator therefore stays as wired before.
        let plan = AuthPlan::derive(package, None);
        match apply_host_load(&mut engine, &mut clock, package, &plan) {
            Ok((completion, router, routing)) => {
                self.reinstate(Loaded { host: SurfaceHost::new(engine, router, clock), routing, blobs });
                self.reopen_connections();
                Ok(Observation {
                    outcome: Outcome::Ok,
                    value: None,
                    completion: Some(completion),
                    extra: Default::default(),
                })
            }
            Err(observed) => {
                // The engine is unchanged (update is atomic); rebuild over the
                // prior router so later steps still resolve the active package.
                self.reinstate(Loaded {
                    host: SurfaceHost::new(engine, old_router, clock),
                    routing: old_routing,
                    blobs,
                });
                self.reopen_connections();
                Ok(Observation::outcome(observed))
            }
        }
    }

    fn operator(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let Some(call_name) = target.get("call").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`operator` step carries no `call` mutation name"));
        };
        if call_name.contains('.') {
            return Err(AdapterError::unsupported(
                "`operator` on a collection-row mutation (`path.key.mut`) needs the receiver-row \
                 wiring the synthetic operator surface does not carry",
            ));
        }
        let args = target.get("args").cloned().unwrap_or(serde_json::Value::Null);
        let address_text = format!("{}.{call_name}", super::OPERATOR_SURFACE_PREFIX);
        let loaded = self.loaded()?;
        let address = SurfaceAddress::parse(&address_text)
            .map_err(|err| AdapterError::Host(format!("malformed operator address `{address_text}`: {err}")))?;
        let types = loaded.routing.arg_types(&address_text);
        let decoded = wire::decode_args(&args, &types);
        let call = SurfaceCall::new(address, decoded);
        let outcome = loaded.host.operator_call(&call).map_err(host_fault)?;
        Ok(observe_call(&outcome))
    }

    fn operation_status(&mut self, id: &str) -> Result<Observation, AdapterError> {
        let key = self.op_keys.get(id).cloned();
        let loaded = self.loaded()?;
        let status = key.map_or(OperationStatus::Unknown, |key| loaded.host.operation_status(&key));
        Ok(Observation::ok(Some(operation_status_value(&status))))
    }

    fn manifest(&mut self, connection: &str, context: Option<&str>) -> Result<Observation, AdapterError> {
        self.ensure_connection(connection);
        let loaded = self.loaded()?;
        let surfaces = loaded.host.manifest(connection, context).map_err(host_fault)?;
        let surfaces: Vec<serde_json::Value> =
            surfaces.into_iter().map(serde_json::Value::String).collect();
        Ok(Observation::ok(Some(serde_json::json!({ "surfaces": surfaces }))))
    }

    fn resume(
        &mut self,
        connection: &str,
        address: &str,
        watch_id: &str,
        from: CommitSeq,
    ) -> Result<Observation, AdapterError> {
        self.ensure_connection(connection);
        let loaded = self.loaded()?;
        let parsed = SurfaceAddress::parse(address)
            .map_err(|err| AdapterError::Host(format!("malformed resume address `{address}`: {err}")))?;
        let singular = loaded.routing.is_singular_view(address);
        let resume = SurfaceResume::new(parsed, watch_id.to_owned(), from);
        let subscription = loaded.host.resume(connection, &resume).map_err(host_fault)?;
        Ok(observe_subscription(&subscription, singular))
    }

    fn authenticate_op(
        &mut self,
        connection: &str,
        payload: &serde_json::Value,
        context: Option<&str>,
    ) -> Result<Observation, AdapterError> {
        self.ensure_connection(connection);
        let loaded = self.loaded()?;
        let Some(mut request) = super::resolve_authenticate(loaded, payload) else {
            // A selection that resolves no wired role leaves the context unbound;
            // the denial is the observable outcome (§11.4).
            return Ok(Observation::outcome(Outcome::Denied));
        };
        if let Some(context) = context {
            request = request.as_context(context.to_owned());
        }
        match loaded.host.authenticate(connection, &request).map_err(host_fault)? {
            AuthResult::Bound => Ok(Observation::ok(None)),
            AuthResult::Denied(_) => Ok(Observation::outcome(Outcome::Denied)),
        }
    }

    fn export(&mut self) -> Result<Vec<u8>, AdapterError> {
        let loaded = self.loaded()?;
        loaded.host.export().map_err(host_fault)
    }

    fn import(&mut self, bytes: &[u8], policy: &[ImportRelation]) -> Result<Observation, AdapterError> {
        let loaded = self.loaded()?;
        match loaded.host.import(bytes, policy) {
            Ok(report) => Ok(Observation {
                outcome: Outcome::Ok,
                value: Some(import_value(report.relation, report.applied, None)),
                completion: Some(if report.applied { Completion::Committed } else { Completion::Unchanged }),
                extra: Default::default(),
            }),
            Err(error) => Ok(Observation::outcome(import_error_outcome(&error))),
        }
    }

    fn reconcile(
        &mut self,
        base: &[u8],
        incoming: &[u8],
        _policy: &[ImportRelation],
    ) -> Result<Observation, AdapterError> {
        let loaded = self.loaded()?;
        match loaded.host.reconcile(base, incoming) {
            Ok(outcome) => {
                // §19.9 `reconcile` computes but never activates the merge, so a
                // clean merge reports `applied: true` as its computed disposition
                // while committed state is left for a host correction to move.
                let conflicts: Vec<serde_json::Value> = outcome
                    .conflicts
                    .iter()
                    .map(|conflict| serde_json::json!({ "coordinate": conflict.coordinate }))
                    .collect();
                Ok(Observation {
                    outcome: Outcome::Ok,
                    value: Some(import_value(ImportRelation::Merge, outcome.is_clean(), Some(conflicts))),
                    completion: Some(Completion::Unchanged),
                    extra: Default::default(),
                })
            }
            Err(error) => Ok(Observation::outcome(import_error_outcome(&error))),
        }
    }

    fn blob_put(&mut self, spec: &super::blobs::BlobPutSpec) -> Result<Observation, AdapterError> {
        self.ensure_connection(&spec.connection);
        match &mut self.state {
            State::Loaded(loaded) => super::blobs::put(loaded, &mut self.blob_digests, spec),
            State::Failed(message) => Err(AdapterError::LoadFailed(message.clone())),
        }
    }

    fn connector_set(&mut self, spec: &super::blobs::ConnectorSetSpec) -> Result<Observation, AdapterError> {
        match &mut self.state {
            State::Loaded(loaded) => super::blobs::connector_set(loaded, &self.blob_digests, spec),
            State::Failed(message) => Err(AdapterError::LoadFailed(message.clone())),
        }
    }

    fn provider_set(&mut self, spec: &super::keyrings::ProviderSetSpec) -> Result<Observation, AdapterError> {
        // §17.9: reconfigure the engine keyring's backing provider. Reaching
        // `&mut Engine` requires rebuilding the host over its engine; committed
        // state, connections, and live subscriptions are preserved across it, so a
        // subscription opened before the fault (a keyring metadata watch) still
        // reads after it.
        self.rebuild_engine(|engine| spec.apply(engine))?;
        Ok(Observation::ok(None))
    }

    fn keyring_admin(&mut self, spec: &super::keyrings::KeyringAdminSpec) -> Result<Observation, AdapterError> {
        // §17.3/§17.4: drive the lifecycle transition against the engine's
        // self-provisioned ring. Reaching `&mut Engine` (for `keyring_admin`)
        // rebuilds the host over its engine, preserving committed state,
        // connections, and live subscriptions — so a `/ring.$*` metadata watch
        // opened after the transition reads the new version view.
        self.rebuild_engine(|engine| spec.apply(engine))
    }
}

/// Run [`Engine::update`] for a reloaded `package`, injecting the same synthetic
/// views/mutations a fresh load would and rebinding the router. Tries the richest
/// surface lift first, falling back to fewer synthetic declarations (exactly as
/// the initial load does) before giving up.
fn apply_host_load<S: InstanceStore>(
    engine: &mut liasse_runtime::Engine<S>,
    clock: &mut SurfaceClock,
    package: &serde_json::Value,
    plan: &AuthPlan,
) -> Result<(Completion, liasse_surface::SurfaceRouter, Routing), Outcome> {
    let lift = SurfaceLift::derive(package);
    let mut attempts = vec![lift.clone()];
    if !lift.views_only().is_empty() {
        attempts.push(lift.views_only());
    }
    if !lift.is_empty() {
        attempts.push(SurfaceLift::default());
    }
    let mut last = Outcome::Error;
    for attempt in attempts {
        let Some(definition) = super::prepared_definition(package, plan, &attempt) else {
            continue;
        };
        let before = engine.head();
        match engine.update(&definition, clock) {
            Ok(_) => {
                let completion =
                    if engine.head() == before { Completion::Unchanged } else { Completion::Committed };
                match super::router::build(engine.model(), package, plan, &attempt) {
                    Ok((router, mut routing)) => {
                        routing.load_view_param_types(engine);
                        return Ok((completion, router, routing));
                    }
                    Err(_) => last = Outcome::Error,
                }
            }
            Err(error) => last = update_outcome(&error),
        }
    }
    Err(last)
}

/// Map an [`Engine::update`] failure to the harness outcome class (§9.4, §20).
fn update_outcome(error: &liasse_runtime::UpdateError) -> Outcome {
    use liasse_runtime::UpdateError as U;
    match error {
        U::Rejected(_) | U::Incompatible(_) => Outcome::Rejected,
        U::Engine(_) => Outcome::Invalid,
    }
}

/// The `{ relation, applied, [conflicts] }` value an import/reconcile step renders
/// (§19.8/§19.9 result shape). The relation is the canonical snake-case token.
fn import_value(relation: ImportRelation, applied: bool, conflicts: Option<Vec<serde_json::Value>>) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("relation".to_owned(), serde_json::Value::String(relation_token(relation).to_owned()));
    object.insert("applied".to_owned(), serde_json::Value::Bool(applied));
    if let Some(conflicts) = conflicts {
        object.insert("conflicts".to_owned(), serde_json::Value::Array(conflicts));
    }
    serde_json::Value::Object(object)
}

/// The canonical wire token for a movement relation (§19.8).
fn relation_token(relation: ImportRelation) -> &'static str {
    match relation {
        ImportRelation::SamePoint => "same_point",
        ImportRelation::FastForward => "fast_forward",
        ImportRelation::Rollback => "rollback",
        ImportRelation::Merge => "merge",
        ImportRelation::Unrelated => "unrelated",
    }
}

/// The `{ status, [frontier], [commit] }` value an `operation_status` step renders
/// (§12.3 retained status). A `committed` record carries its frontier and commit
/// sequence; `unknown` reveals nothing beyond the status token.
fn operation_status_value(status: &OperationStatus) -> serde_json::Value {
    match status {
        OperationStatus::Committed { frontier, commit } => serde_json::json!({
            "status": "committed",
            "frontier": frontier.get(),
            "commit": commit.get(),
        }),
        OperationStatus::Unchanged { frontier } => serde_json::json!({
            "status": "unchanged",
            "frontier": frontier.get(),
        }),
        OperationStatus::Rejected => serde_json::json!({ "status": "rejected" }),
        OperationStatus::Unknown => serde_json::json!({ "status": "unknown" }),
    }
}

/// Map an artifact/import failure to the harness outcome class: a failed recursive
/// `.liasse` verification (§19.8) or malformed section is a static `invalid`; a
/// store/rebuild fault while moving state is an `error`.
fn import_error_outcome(error: &ImportError) -> Outcome {
    match error {
        ImportError::Artifact(_) | ImportError::Corrupt(_) => Outcome::Invalid,
        ImportError::Engine(_) => Outcome::Error,
    }
}
