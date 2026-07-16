//! The conformance adapter: a [`Driver`] over the real Liasse stack.
//!
//! [`ScenarioAdapter`] loads a case's package into a runtime [`Engine`] over an
//! in-memory store and drives the core client verbs through a [`SurfaceHost`] —
//! `connect`, `call` (with `operation_id`), `watch`/`expect_view`, and the
//! virtual-clock `advance_time`. The adapter only translates: it decodes wire
//! arguments to typed values, routes a dotted address, and renders the observed
//! outcome back to the harness vocabulary; the engine decides every spec
//! question. There is no case-specific special-casing.
//!
//! # Determinism
//!
//! The store starts empty, the clock is fixed at the FORMAT.md epoch
//! (`2026-01-01T00:00:00Z`, micro-second precision), and generated identifiers
//! come from the runtime's per-call-site derivation over a monotone seed, so a
//! `$any:uuid`/`$bind` value is stable across a run and a re-run.
//!
//! # This phase's reach
//!
//! Public surfaces, calls, and named-view watches route end to end. The long tail
//! — authenticated/role calls, artifact export/import/reconcile, blob and keyring
//! operations, module lifecycle, the host-operator entry, restart durability —
//! needs host wiring or store access the current layer does not expose, so those
//! [`OpRequest`] kinds report a harness skip (never a panic), leaving the outcome
//! for the triage loop to harden. Load failures likewise skip every step.

mod auth;
mod error;
pub mod lift;
mod ops;
mod router;
mod shape;
mod wire;

use std::collections::BTreeMap;

use liasse_ident::InstanceId;
use liasse_runtime::{Engine, Precision, Timestamp};
use liasse_store::{InstanceStore, MemoryStore};
use liasse_surface::{
    AuthSelection, Authenticate, Credential, Subscription, SurfaceAddress, SurfaceCall,
    SurfaceError, SurfaceHost, SurfaceWatch, Window, VirtualClock as SurfaceClock,
};
use liasse_value::{Text, Value};

use crate::case::{Case, PackageSet};
use crate::clock::VirtualClock;
use crate::contract::{ConnectRequest, Driver, Observation};
use crate::id::{ConnectionId, WatchId};
use crate::outcome::{Completion, Outcome};
use crate::request::OpRequest;

pub use error::AdapterError;
use router::Routing;

/// Micro-seconds from the Unix epoch to `2026-01-01T00:00:00Z`, the FORMAT.md
/// virtual-clock start.
const EPOCH_MICROS: i128 = 1_767_225_600_000_000;

/// The implicit connection the executor opens for a single-client case.
const IMPLICIT_CONNECTION: &str = "$default";

/// Provisions a fresh, empty instance store for one scenario case. The memory
/// runner uses [`MemoryProvision`]; the PostgreSQL runner implements this over a
/// self-provisioning `PgStoreFactory`, so the *identical* scenario battery drives
/// both backends and any verdict divergence is a store-contract bug, not a
/// harness difference.
pub trait StoreProvision {
    /// The store this provisioner creates.
    type Store: InstanceStore;

    /// Create a fresh, empty store at genesis for `instance`. A returned error
    /// message becomes the case's load failure, so every step skips with it
    /// (exactly as a compile failure does) rather than aborting the run.
    fn provision(&mut self, instance: InstanceId) -> Result<Self::Store, String>;
}

/// The default in-memory provisioner: a `BTreeMap`-backed [`MemoryStore`] per
/// instance, provisioning of which cannot fail.
#[derive(Debug, Default)]
pub struct MemoryProvision;

impl StoreProvision for MemoryProvision {
    type Store = MemoryStore;

    fn provision(&mut self, instance: InstanceId) -> Result<Self::Store, String> {
        Ok(MemoryStore::new(instance))
    }
}

/// A loaded case's live stack: the surface host over the engine, and the routing
/// tables the adapter decodes arguments against.
struct Loaded<S: InstanceStore> {
    host: SurfaceHost<S>,
    routing: Routing,
}

/// Either the loaded stack or the reason the package did not load. The loaded
/// arm is boxed: it holds the whole surface host, far larger than the failure
/// message.
enum State<S: InstanceStore> {
    Loaded(Box<Loaded<S>>),
    Failed(String),
}

/// A [`Driver`] that runs one scenario case against the real runtime and surface
/// stack over a store the caller provisions (in-memory by default, or any
/// [`StoreProvision`] backend such as PostgreSQL).
pub struct ScenarioAdapter<S: InstanceStore> {
    state: State<S>,
    /// Which connection each open subscription lives on, so an `expect_view`
    /// (which names only the subscription) reads the right connection's cache.
    watch_conns: BTreeMap<String, String>,
    /// Which open subscriptions watch a singular view (§12.2), so a later
    /// `expect_view` renders that subscription's result as a JSON object.
    watch_singular: BTreeMap<String, bool>,
    /// The connection ids currently open on the host. A §9.2 `host_load` rebuilds
    /// the host and re-opens these (a lifecycle load does not drop clients),
    /// while a §22 restart clears them (its volatile connections are dropped).
    open_connections: std::collections::BTreeSet<String>,
    /// The adapter-owned virtual clock, used to compute the absolute instant an
    /// `advance_time` moves the surface clock to.
    clock: VirtualClock,
}

impl ScenarioAdapter<MemoryStore> {
    /// Build an adapter for `case` over a fresh in-memory store, loading its
    /// (root) package into a fresh engine. A load failure is retained so every
    /// step skips with its reason.
    #[must_use]
    pub fn build(case: &Case) -> Self {
        Self::build_with(&mut MemoryProvision, case)
    }
}

impl<S: InstanceStore> ScenarioAdapter<S> {
    /// Build an adapter for `case` over a store obtained from `provision`,
    /// loading its (root) package into a fresh engine. A load failure — whether
    /// the store could not be provisioned or the definition did not compile — is
    /// retained so every step skips with its reason. The provisioner is borrowed
    /// only for the load, so one provisioner (a PostgreSQL factory, say) serves
    /// every case in a run.
    #[must_use]
    pub fn build_with<P: StoreProvision<Store = S>>(provision: &mut P, case: &Case) -> Self {
        let state = match Self::load(provision, case) {
            Ok(loaded) => State::Loaded(Box::new(loaded)),
            Err(message) => State::Failed(message),
        };
        Self {
            state,
            watch_conns: BTreeMap::new(),
            watch_singular: BTreeMap::new(),
            open_connections: std::collections::BTreeSet::new(),
            clock: VirtualClock::new(),
        }
    }

    fn load<P: StoreProvision<Store = S>>(provision: &mut P, case: &Case) -> Result<Loaded<S>, String> {
        let package = root_package(&case.packages).ok_or_else(|| "case declares no package".to_owned())?;
        // §11 host wiring: reconstruct the authenticators/roles a host-free case
        // declares, and inject the synthetic views its `$actor`/`$members`
        // selections resolve through, before the engine compiles the model.
        let plan = auth::AuthPlan::derive(package);
        // §10.1/§10.2 host wiring: lift each inline surface `$view`/`$mut` into a
        // synthetic top-level declaration the engine can compile, so the router
        // binds a named runtime view/mutation rather than dropping the surface.
        // Try the richest wiring first; if a lifted inline mutation is one the
        // model cannot yet compile (e.g. an uninferrable parameter), fall back to
        // fewer synthetic declarations rather than regress a case that loaded
        // before — an unlifted inline call then resolves `denied`, as it did.
        let lift = lift::SurfaceLift::derive(package);
        let mut attempts = vec![lift.clone()];
        if !lift.views_only().is_empty() {
            attempts.push(lift.views_only());
        }
        if !lift.is_empty() {
            attempts.push(lift::SurfaceLift::default());
        }
        let mut last_error = None;
        for attempt in attempts {
            match Self::load_with(provision, case, package, &plan, &attempt) {
                Ok(loaded) => return Ok(loaded),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| "case did not load".to_owned()))
    }

    /// Load `package` with a specific set of `lift`ed synthetic declarations.
    fn load_with<P: StoreProvision<Store = S>>(
        provision: &mut P,
        case: &Case,
        package: &serde_json::Value,
        plan: &auth::AuthPlan,
        lift: &lift::SurfaceLift,
    ) -> Result<Loaded<S>, String> {
        let definition = prepared_definition(package, plan, lift)
            .ok_or_else(|| "prepared definition did not serialize".to_owned())?;
        let store = provision.provision(InstanceId::new(case.name.clone()))?;
        let mut clock = SurfaceClock::new(EPOCH_MICROS, Precision::Micros);
        let engine = Engine::load(store, &definition, &mut clock).map_err(|err| err.to_string())?;
        let (router, routing) =
            router::build(engine.model(), package, plan, lift).map_err(|err| err.to_string())?;
        let host = SurfaceHost::new(engine, router, clock);
        Ok(Loaded { host, routing })
    }

    fn loaded(&mut self) -> Result<&mut Loaded<S>, AdapterError> {
        match &mut self.state {
            State::Loaded(loaded) => Ok(&mut **loaded),
            State::Failed(message) => Err(AdapterError::LoadFailed(message.clone())),
        }
    }
}

impl<S: InstanceStore> Driver for ScenarioAdapter<S> {
    type Error = AdapterError;

    fn connect(&mut self, request: ConnectRequest) -> Result<Observation, Self::Error> {
        let connection = request.connection.to_string();
        self.open_connections.insert(connection.clone());
        let loaded = self.loaded()?;
        loaded.host.connect(connection.clone());
        // §11.4: bind the authenticated context on the connection so later role
        // calls run under it. Only the host-free `{ role, auth, credential }`
        // shape is honored; a payload the wiring does not cover (a host-namespace
        // verifier, a `$session` flow) leaves the connection unauthenticated, so
        // a later role call is denied rather than silently admitted. A refused
        // authentication is not a connect failure — the denial surfaces on the
        // call the case asserts against.
        if let Some(request) = request.authenticate.as_ref().and_then(parse_authenticate) {
            let _ = loaded.host.authenticate(&connection, &request);
        }
        Ok(Observation::ok(None))
    }

    fn disconnect(&mut self, connection: &ConnectionId) -> Result<Observation, Self::Error> {
        self.open_connections.remove(&connection.to_string());
        let loaded = self.loaded()?;
        loaded.host.disconnect(&connection.to_string());
        Ok(Observation::ok(None))
    }

    fn call(&mut self, request: crate::contract::CallRequest) -> Result<Observation, Self::Error> {
        let connection = connection_name(request.on.as_ref());
        let loaded = self.loaded()?;
        let address = SurfaceAddress::parse(&request.target)
            .map_err(|err| AdapterError::Host(format!("malformed address `{}`: {err}", request.target)))?;
        let types = loaded.routing.arg_types(&request.target);
        let args = wire::decode_args(&request.args, &types);
        let mut call = SurfaceCall::new(address, args);
        if let Some(operation_id) = &request.operation_id {
            call = call.with_operation_id(operation_id.clone());
        }
        let outcome = loaded.host.call(&connection, &call).map_err(host_fault)?;
        Ok(observe_call(&outcome))
    }

    fn watch(&mut self, request: crate::contract::WatchRequest) -> Result<Observation, Self::Error> {
        let connection = connection_name(request.on.as_ref());
        let watch_id = request.id.to_string();
        let (observation, singular) = match &mut self.state {
            State::Loaded(loaded) => {
                let address = SurfaceAddress::parse(&request.target).map_err(|err| {
                    AdapterError::Host(format!("malformed address `{}`: {err}", request.target))
                })?;
                let mut watch = SurfaceWatch::new(address, watch_id.clone());
                if let Some(window) = request.window.as_ref().and_then(build_window) {
                    watch = watch.with_window(window);
                }
                // §12.2: a singular view (a root/struct projection or an
                // aggregate) delivers one object; a collection view a row array.
                let singular = loaded.routing.is_singular_view(&request.target);
                let subscription = loaded.host.watch(&connection, &watch).map_err(host_fault)?;
                (observe_subscription(&subscription, singular), singular)
            }
            State::Failed(message) => return Err(AdapterError::LoadFailed(message.clone())),
        };
        self.watch_conns.insert(watch_id.clone(), connection);
        self.watch_singular.insert(watch_id, singular);
        Ok(observation)
    }

    fn unwatch(&mut self, id: &WatchId) -> Result<Observation, Self::Error> {
        self.watch_conns.remove(&id.to_string());
        self.watch_singular.remove(&id.to_string());
        Ok(Observation::ok(None))
    }

    fn read_view(&mut self, id: &WatchId) -> Result<Observation, Self::Error> {
        let connection = self.watch_conns.get(&id.to_string()).cloned();
        let watch_id = id.to_string();
        let singular = self.watch_singular.get(&watch_id).copied().unwrap_or(false);
        let loaded = self.loaded()?;
        // A bounded subscription reports its windowed rows; an unbounded one its
        // full current result, rendered per its §12.2 delivery shape.
        let value = connection.as_deref().and_then(|conn| {
            loaded
                .host
                .read_window(conn, &watch_id)
                .map(wire::rows_to_json)
                .or_else(|| {
                    loaded.host.read_view(conn, &watch_id).map(|r| wire::view_to_json_shaped(r, singular))
                })
        });
        Ok(Observation::ok(value))
    }

    fn advance_time(
        &mut self,
        duration: &crate::clock::Iso8601Duration,
    ) -> Result<Observation, Self::Error> {
        let instant = self.clock.advance(duration);
        let now = Timestamp::new(i128::from(instant.unix_micros()), Precision::Micros);
        let loaded = self.loaded()?;
        // §14.1/§22.6: advancing time is not a commit, yet a row leaving its
        // half-open active interval must re-evaluate every live view at the new
        // instant. `advance_time` moves both the session-expiry clock (§11.7) and
        // the engine's bucket clock (§14) and sweeps every open subscription.
        loaded.host.advance_time(now).map_err(host_fault)?;
        Ok(Observation::ok(None))
    }

    fn restart(&mut self) -> Result<Observation, Self::Error> {
        self.drive_restart()
    }

    fn op(&mut self, request: &OpRequest) -> Result<Observation, Self::Error> {
        self.drive_op(request)
    }
}

/// The connection a call/watch runs on, defaulting to the implicit single-client
/// connection when the executor left `on` unset.
fn connection_name(on: Option<&ConnectionId>) -> String {
    on.map_or_else(|| IMPLICIT_CONNECTION.to_owned(), ToString::to_string)
}

/// The root package definition of a case: the sole `package`, or the `root`
/// label of a `packages` map (falling back to its first entry).
fn root_package(packages: &PackageSet) -> Option<&serde_json::Value> {
    match packages {
        PackageSet::Single(package) => Some(package),
        PackageSet::Multi { packages, root } => root
            .as_ref()
            .and_then(|label| packages.get(label))
            .or_else(|| packages.values().next()),
    }
}

/// Prepare a package for compilation: clone it, inject the plan's synthetic
/// `$actor`/`$members` views, splice in the lifted inline surface declarations,
/// and serialize to the definition string the engine loads or updates against.
/// The initial load and a §9.2 `host_load` share this exact preparation, so both
/// compile the same wiring. Returns `None` only if serialization fails.
fn prepared_definition(
    package: &serde_json::Value,
    plan: &auth::AuthPlan,
    lift: &lift::SurfaceLift,
) -> Option<String> {
    let mut definition = package.clone();
    inject_synthetic_views(&mut definition, plan);
    if let Some(model) = definition.get_mut("$model").and_then(serde_json::Value::as_object_mut) {
        lift.inject(model);
    }
    serde_json::to_string(&definition).ok()
}

/// Inject the plan's synthetic `$actor`/`$members` views into a package's
/// `$model` block, so the surface layer's [`RowSource`]s resolve them by name.
/// A package with no `$model` object (or an inactive plan) is left untouched.
///
/// [`RowSource`]: liasse_surface::RowSource
fn inject_synthetic_views(package: &mut serde_json::Value, plan: &auth::AuthPlan) {
    if !plan.is_active() {
        return;
    }
    let Some(model) = package.get_mut("$model").and_then(serde_json::Value::as_object_mut) else {
        return;
    };
    for (name, expr) in plan.synthetic_views() {
        model.insert(name.clone(), serde_json::json!({ "$view": expr }));
    }
}

/// Parse a verbatim `authenticate` payload into a surface [`Authenticate`]
/// request. Only the host-free `{ role, auth, credential }` shape is recognized;
/// a payload missing any of the three (a session/host-namespace flow) yields
/// `None`, leaving the connection unauthenticated.
fn parse_authenticate(payload: &serde_json::Value) -> Option<Authenticate> {
    let object = payload.as_object()?;
    let role = object.get("role")?.as_str()?;
    let auth = object.get("auth")?.as_str()?;
    let credential = object.get("credential")?.as_str()?;
    let selection =
        AuthSelection::new(auth, Credential::new(Value::Text(Text::new(credential.to_owned()))));
    Some(Authenticate::new(role, selection))
}

/// Build a bounded window (§12.2) from a verbatim `window` spec. Handles the
/// `$size` bound with a `$first`/`$last` anchor, a concrete occurrence anchor (a
/// row key identity), and the `$slide` flag. A concrete anchor is resolved to the
/// row's stable [`RowId`] the same way the view materializer keys it (D.2): the
/// anchor's canonical key text. The engine then requires that occurrence be
/// present when the window opens (§12.2).
fn build_window(spec: &serde_json::Value) -> Option<Window> {
    let object = spec.as_object()?;
    let size = usize::try_from(object.get("$size")?.as_u64()?).ok()?;
    let slide = object.get("$slide").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let window = match object.get("$anchor") {
        None => Window::first(size),
        Some(serde_json::Value::String(anchor)) => match anchor.as_str() {
            "$first" => Window::first(size),
            "$last" => Window::last(size),
            _ => Window::anchored(size, anchor_row_id(anchor)),
        },
        Some(_) => return None,
    };
    Some(if slide { window.sliding() } else { window })
}

/// The stable [`RowId`] a concrete window anchor names. A top-level view row is
/// keyed by its source row's canonical key text (Annex D.2); the anchor carries
/// that key in wire form, so its [`KeyText`] over a `text` value reproduces the
/// same identity the view materializer assigns. A key value carrying reserved
/// characters is escaped identically on both sides through the one D.2 codec.
fn anchor_row_id(anchor: &str) -> liasse_expr::RowId {
    let value = Value::Text(Text::new(anchor.to_owned()));
    let text = liasse_ident::KeyText::from_key_values(std::slice::from_ref(&value))
        .map(|key| key.as_str().to_owned())
        .unwrap_or_else(|_| anchor.to_owned());
    liasse_expr::RowId::keyed(text)
}

/// Render a call outcome to a harness observation, projecting the response value
/// to canonical strict-JSON and reporting the success completion.
fn observe_call(outcome: &liasse_surface::SurfaceOutcome) -> Observation {
    use liasse_surface::SurfaceOutcome as S;
    match outcome {
        S::Committed { response, .. } => Observation {
            outcome: Outcome::Ok,
            value: response.as_ref().map(wire::response_to_json),
            completion: Some(Completion::Committed),
            extra: serde_json::Map::new(),
        },
        S::Unchanged { response, .. } => Observation {
            outcome: Outcome::Ok,
            value: response.as_ref().map(wire::response_to_json),
            completion: Some(Completion::Unchanged),
            extra: serde_json::Map::new(),
        },
        S::Rejected(_) => Observation::outcome(Outcome::Rejected),
        S::Denied(_) => Observation::outcome(Outcome::Denied),
    }
}

/// Render a subscription result to a harness observation: the initial view rows
/// as strict-JSON (an object for a singular view, §12.2), or the refusal class.
fn observe_subscription(subscription: &Subscription, singular: bool) -> Observation {
    match subscription {
        Subscription::Init(result) => Observation::ok(Some(wire::view_to_json_shaped(result, singular))),
        Subscription::Window(rows) => Observation::ok(Some(wire::rows_to_json(rows))),
        Subscription::Denied(_) => Observation::outcome(Outcome::Denied),
        Subscription::Failed(_) => Observation::outcome(Outcome::Error),
    }
}

/// Map a surface transport fault to a harness skip.
fn host_fault(error: SurfaceError) -> AdapterError {
    AdapterError::Host(error.to_string())
}
