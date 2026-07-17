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
//! # Instances and sandboxes
//!
//! A case drives one *base* instance over its store. An `in_sandbox` group opens
//! an isolated *sandbox* instance: the executor signals
//! [`Driver::enter_sandbox`]/[`Driver::exit_sandbox`] around the group, so the
//! adapter pushes a fresh in-memory instance on entry and pops it on exit — a
//! §19.10 `restore` inside the group activates that throwaway instance and never
//! touches the outer one. Both instances are a [`Runtime`], reached through the
//! store-erased [`Instance`] trait, so every verb is written once. Exported
//! `.liasse` bytes live in the adapter's [`artifacts`](ScenarioAdapter::artifacts)
//! table (shared across the sandbox boundary), so an `export` step feeds a later
//! `import`/`reconcile` step exactly as the §19.5 byte stream is the interchange.
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
//! Public surfaces, calls, named-view watches, `export`/`import`/`reconcile`, the
//! host-operator entry, and §9.2 lifecycle loads route end to end. The long tail —
//! blob and keyring operations, module lifecycle, full `.liasse` archive assembly
//! and tampering — still needs host wiring the current layer does not expose, so
//! those [`OpRequest`] kinds report a harness skip (never a panic), leaving the
//! outcome for the triage loop to harden. Load failures likewise skip every step.

mod auth;
mod blobs;
mod error;
mod keyrings;
pub mod lift;
mod namespaces;
mod ops;
mod router;
mod runtime;
mod shape;
mod wire;

use liasse_ident::InstanceId;
use liasse_runtime::{ContractRef, Engine, Precision, Registry};
use liasse_store::{InstanceStore, MemoryStore};
use liasse_surface::{
    Authenticate, AuthSelection, Credential, SurfaceHost, Subscription, SurfaceError,
    Window, VirtualClock as SurfaceClock,
};
use liasse_value::{Json, Struct, Text, Type, Value};

use crate::case::{Case, PackageSet};
use crate::contract::{ConnectRequest, Driver, Observation};
use crate::id::{ConnectionId, WatchId};
use crate::outcome::{Completion, Outcome};
use crate::request::OpRequest;

pub use error::AdapterError;
use router::Routing;
use runtime::{Instance, Runtime};

/// Micro-seconds from the Unix epoch to `2026-01-01T00:00:00Z`, the FORMAT.md
/// virtual-clock start.
const EPOCH_MICROS: i128 = 1_767_225_600_000_000;

/// The implicit connection the executor opens for a single-client case.
const IMPLICIT_CONNECTION: &str = "$default";

/// The synthetic public surface name the load pass injects into an operator case,
/// exposing every top-level `$mut` so a §23.5 operator transition can resolve a
/// bare model-mutation name through the ordinary router. Reserved (double-
/// underscored, ASCII) so it never collides with a package-declared surface.
/// A declaration name must begin with an ASCII letter (M-SURFACE), so the
/// synthetic name starts with a letter and is otherwise reserved-looking to avoid
/// colliding with a package-declared surface.
const OPERATOR_SURFACE: &str = "hostoperator__";

/// The `public.<surface>` prefix an operator target resolves under; the operator
/// step's bare `call` name is appended as the call segment.
pub(super) const OPERATOR_SURFACE_PREFIX: &str = "public.hostoperator__";

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
pub(super) struct Loaded<S: InstanceStore> {
    host: SurfaceHost<S>,
    routing: Routing,
    /// The §18 blob wiring (registered field names, store→connector map) the
    /// blob steps resolve connectors and fields through.
    blobs: blobs::BlobWiring,
}

/// Either the loaded stack or the reason the package did not load. The loaded
/// arm is boxed: it holds the whole surface host, far larger than the failure
/// message.
pub(super) enum State<S: InstanceStore> {
    Loaded(Box<Loaded<S>>),
    Failed(String),
}

/// The wiring a sandbox restore reproduces: the instance incarnation to restore
/// as (so a restored artifact classifies against the base's history, §19.8), and
/// the package plus successful surface lift the router rebinds against. Captured
/// from the base load so a §19.10 restore reconstructs the identical routing over
/// the artifact's definition.
struct LoadContext {
    instance: InstanceId,
    package: serde_json::Value,
    /// The case's `hosts` block (§11.3 verifier tables), replayed so a sandbox
    /// restore reconstructs the identical authentication wiring.
    hosts: Option<serde_json::Value>,
    lift: lift::SurfaceLift,
}

/// A [`Driver`] that runs one scenario case against the real runtime and surface
/// stack over a store the caller provisions (in-memory by default, or any
/// [`StoreProvision`] backend such as PostgreSQL).
pub struct ScenarioAdapter<S: InstanceStore> {
    /// The base instance over the case's store.
    base: Runtime<S>,
    /// The stack of open `in_sandbox` instances, each over a throwaway in-memory
    /// store; the last is the active one.
    sandboxes: Vec<Runtime<MemoryStore>>,
    /// The artifact each open sandbox was restored from, parallel to `sandboxes`
    /// — the shared ancestor an artifact exported inside that sandbox diverged
    /// from, so a later `reconcile` can name the §19.9 merge base the corpus step
    /// leaves implicit.
    sandbox_origins: Vec<Option<String>>,
    /// Exported `.liasse` bytes by label, shared across the sandbox boundary so an
    /// `export` feeds a later `import`/`reconcile` (§19.5).
    artifacts: std::collections::BTreeMap<String, Vec<u8>>,
    /// The §19.9 merge base of each exported artifact: the shared ancestor it
    /// diverged from (the sandbox origin at export time), keyed by artifact label.
    artifact_origin: std::collections::BTreeMap<String, String>,
    /// The base load's wiring, replayed when a sandbox restores an artifact.
    load_ctx: LoadContext,
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
        let instance = InstanceId::new(case.name.clone());
        let (state, load_ctx) = match Self::load(provision, case) {
            Ok((loaded, package, lift)) => (
                State::Loaded(Box::new(loaded)),
                LoadContext { instance, package, hosts: case.hosts.clone(), lift },
            ),
            Err(message) => (
                State::Failed(message),
                LoadContext {
                    instance,
                    package: serde_json::Value::Null,
                    hosts: None,
                    lift: lift::SurfaceLift::default(),
                },
            ),
        };
        Self {
            base: Runtime::new(state),
            sandboxes: Vec::new(),
            sandbox_origins: Vec::new(),
            artifacts: std::collections::BTreeMap::new(),
            artifact_origin: std::collections::BTreeMap::new(),
            load_ctx,
        }
    }

    /// Load the case's root package, returning the loaded stack together with the
    /// augmented package and the successful surface lift, so a sandbox restore can
    /// replay the identical wiring.
    fn load<P: StoreProvision<Store = S>>(
        provision: &mut P,
        case: &Case,
    ) -> Result<(Loaded<S>, serde_json::Value, lift::SurfaceLift), String> {
        let base = root_package(&case.packages).ok_or_else(|| "case declares no package".to_owned())?;
        // §23.5: an operator target is a bare model `$mut` name, which the router
        // resolves only when it backs an exposed surface. Inject a synthetic public
        // surface exposing every top-level `$mut` so the operator entry resolves,
        // confined to cases that actually run an operator step so no other case's
        // exposed-surface set changes.
        let augmented;
        let package: &serde_json::Value = if case_uses_operator(case) {
            let mut owned = base.clone();
            inject_operator_surface(&mut owned);
            augmented = owned;
            &augmented
        } else {
            base
        };
        // §11 host wiring: reconstruct the authenticators/roles a host-free case
        // declares, and inject the synthetic views its `$actor`/`$members`
        // selections resolve through, before the engine compiles the model.
        let plan = auth::AuthPlan::derive(package, case.hosts.as_ref());
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
                Ok(loaded) => return Ok((loaded, package.clone(), attempt)),
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
        let engine = load_engine(store, &definition, &mut clock, package, case.hosts.as_ref())
            .map_err(|err| err.to_string())?;
        let (router, mut routing) =
            router::build(engine.model(), package, plan, lift).map_err(|err| err.to_string())?;
        routing.load_view_param_types(&engine);
        let mut host = SurfaceHost::new(engine, router, clock);
        // §18: compose a blob host per declared blob field over the case's
        // `hosts.connectors` and `$data` store rows, so a `blob_put`/`blob_get`
        // step drives the real §18 upload/fetch through the surface call path.
        let blobs = blobs::provision(&mut host, package, case.hosts.as_ref());
        Ok(Loaded { host, routing, blobs })
    }

    /// The active instance every core verb drives: the top sandbox if one is open,
    /// otherwise the base.
    fn active(&mut self) -> &mut dyn Instance {
        if let Some(sandbox) = self.sandboxes.last_mut() {
            return sandbox;
        }
        &mut self.base
    }
}

impl<S: InstanceStore> Driver for ScenarioAdapter<S> {
    type Error = AdapterError;

    fn connect(&mut self, request: ConnectRequest) -> Result<Observation, Self::Error> {
        self.active().connect(request)
    }

    fn disconnect(&mut self, connection: &ConnectionId) -> Result<Observation, Self::Error> {
        self.active().disconnect(connection)
    }

    fn call(&mut self, request: crate::contract::CallRequest) -> Result<Observation, Self::Error> {
        self.active().call(request)
    }

    fn watch(&mut self, request: crate::contract::WatchRequest) -> Result<Observation, Self::Error> {
        self.active().watch(request)
    }

    fn unwatch(&mut self, id: &WatchId) -> Result<Observation, Self::Error> {
        self.active().unwatch(id)
    }

    fn read_view(&mut self, id: &WatchId) -> Result<Observation, Self::Error> {
        self.active().read_view(id)
    }

    fn advance_time(
        &mut self,
        duration: &crate::clock::Iso8601Duration,
    ) -> Result<Observation, Self::Error> {
        self.active().advance_time(duration)
    }

    fn restart(&mut self) -> Result<Observation, Self::Error> {
        self.active().restart()
    }

    fn enter_sandbox(&mut self, _name: &str, fresh: bool) -> Result<(), Self::Error> {
        // §19.10: an `in_sandbox` group runs on an isolated instance. A `fresh`
        // group is an independent installation of the case package — its own
        // genesis and incarnation, so an artifact it exports is `unrelated` to the
        // base's history (§19.8). Otherwise push an empty slot a `restore` step
        // activates over a throwaway in-memory store.
        let state = if fresh {
            let foreign = InstanceId::new(format!("{}#sandbox{}", self.load_ctx.instance.as_str(), self.sandboxes.len()));
            match Self::fresh_stack(&self.load_ctx, foreign) {
                Ok(loaded) => State::Loaded(Box::new(loaded)),
                Err(message) => State::Failed(message),
            }
        } else {
            State::Failed(
                "sandbox instance not restored yet (an `in_sandbox` group must `restore` an artifact)".to_owned(),
            )
        };
        self.sandboxes.push(Runtime::new(state));
        self.sandbox_origins.push(None);
        Ok(())
    }

    fn exit_sandbox(&mut self) -> Result<(), Self::Error> {
        self.sandboxes.pop();
        self.sandbox_origins.pop();
        Ok(())
    }

    fn op(&mut self, request: &OpRequest) -> Result<Observation, Self::Error> {
        self.drive_op(request)
    }
}

/// The connection a call/watch runs on, defaulting to the implicit single-client
/// connection when the executor left `on` unset.
pub(super) fn connection_name(on: Option<&ConnectionId>) -> String {
    on.map_or_else(|| IMPLICIT_CONNECTION.to_owned(), ToString::to_string)
}

/// Load `definition` into `store` (§9.2), choosing the requirement-resolution
/// discipline from the package's `$requires` and its `hosts.namespaces` block.
///
/// When the case declares one or more §16 namespace *descriptors* (a `functions`
/// roster, tests/16-host-namespaces/NOTES.md) and every `$requires` entry resolves
/// against them (or the built-in `liasse.cose`), build a [`Registry`] from those
/// descriptors and resolve *strictly* through [`Engine::load_with_hosts`] (§16.2:
/// a missing/incompatible/ambiguous requirement fails load before activation), so
/// a host call in a view/default/verifier type-checks and evaluates against the
/// pinned descriptor. When the requirements name only the built-in cose contract
/// (a keyring package), resolve strictly against the auto-seeded cose namespace
/// with no other registered component; its rings are self-provisioned
/// (adapter/keyrings.rs). Otherwise keep the lenient [`Engine::load`], which defers
/// an unresolved requirement rather than failing it: the §11/§12 host-verifier
/// namespaces a case requires are reconstructed at the auth layer (adapter/auth.rs),
/// not registered as engine components, so lenient is the safe path for them.
fn load_engine<S: InstanceStore, G: liasse_runtime::Generators>(
    store: S,
    definition: &str,
    generator: &mut G,
    package: &serde_json::Value,
    hosts: Option<&serde_json::Value>,
) -> Result<Engine<S>, liasse_runtime::EngineError> {
    let namespaces = namespaces::sim_namespaces(hosts);
    if !namespaces.is_empty() {
        let registry = namespaces::registry(namespaces);
        if requires_resolve_against(package, &registry) {
            return Engine::load_with_hosts(store, definition, generator, registry);
        }
    }
    if requires_only_builtin_cose(package) {
        Engine::load_with_hosts(store, definition, generator, Registry::new())
    } else {
        Engine::load(store, definition, generator)
    }
}

/// Whether every `$requires` entry resolves against `registry` (§16.2), treating
/// the runtime's built-in `liasse.cose` contract as always available. `false` when
/// the package declares no requirements (nothing to resolve strictly) or a
/// requirement names a namespace neither registered nor built in — so the lenient
/// load, which defers it, is chosen instead.
fn requires_resolve_against(package: &serde_json::Value, registry: &Registry) -> bool {
    package
        .get("$requires")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|requires| {
            !requires.is_empty()
                && requires.values().all(|spec| {
                    spec.as_str().is_some_and(|spec| match ContractRef::parse(spec) {
                        Ok(contract) if contract.name().as_str() == "liasse.cose" => true,
                        Ok(contract) => registry.resolve_namespace(&contract).is_ok(),
                        Err(_) => false,
                    })
                })
        })
}

/// Whether the package's `$requires` names only the built-in `liasse.cose`
/// contract — the requirement set the engine resolves with no registered
/// component (it seeds a cose namespace when the registry carries none). `false`
/// when there is no `$requires` (nothing to resolve strictly) or any requirement
/// names another contract (which the adapter does not register, so strict
/// resolution would wrongly fail the load).
fn requires_only_builtin_cose(package: &serde_json::Value) -> bool {
    package
        .get("$requires")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|requires| {
            !requires.is_empty()
                && requires
                    .values()
                    .all(|spec| spec.as_str().is_some_and(|s| s.trim().starts_with("liasse.cose@")))
        })
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
pub(super) fn prepared_definition(
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
/// request. `auth` and `credential` are required; the targeted `role` is the one
/// the payload names, or — when it names none (§11.4 lets a client select an
/// authenticator without naming a role) — a wired role that accepts the selected
/// authenticator, resolved through `routing`. A payload naming an authenticator
/// no wired role accepts yields `None`, leaving the connection unauthenticated;
/// the denial then surfaces on the asserted call.
pub(super) fn parse_authenticate(
    payload: &serde_json::Value,
    routing: &router::Routing,
) -> Option<Authenticate> {
    let object = payload.as_object()?;
    let auth = object.get("auth")?.as_str()?;
    let credential = object.get("credential")?.as_str()?;
    let role = object
        .get("role")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| routing.role_for_auth(auth).map(ToOwned::to_owned))?;
    let selection =
        AuthSelection::new(auth, Credential::new(Value::Text(Text::new(credential.to_owned()))));
    Some(Authenticate::new(role, selection))
}

/// Parse a per-request `auth` selection (§11.4) attached to a `call` step into a
/// surface [`AuthSelection`]. Both `auth` (the authenticator name) and
/// `credential` are required; a payload missing either — a credential with no
/// authenticator named, which §11.4 requires — yields `None`, so the request
/// carries no selection and is denied when it targets a role surface.
pub(super) fn parse_auth_selection(payload: &serde_json::Value) -> Option<AuthSelection> {
    let object = payload.as_object()?;
    let auth = object.get("auth")?.as_str()?;
    let credential = object.get("credential")?.as_str()?;
    Some(AuthSelection::new(auth, Credential::new(Value::Text(Text::new(credential.to_owned())))))
}

/// Build the surface [`Authenticate`] for a `connect`/`authenticate` payload,
/// gating a §17.7 cose authenticator's credential through
/// [`Engine::cose_verify`](Engine::cose_verify) first.
///
/// When the payload's authenticator is a `$verify: "cose.verify(/ring, …)"`
/// authenticator (adapter/auth.rs records these on the router), its credential is
/// a login-minted cose token carried back as a wire object. This reconstructs that
/// token and verifies it against the named ring's accepted versions at the current
/// instant — the acceptance read (§17.7) the surface [`Verifier`](liasse_surface::Verifier)
/// seam cannot reach the engine to perform. A token that verifies is replaced by
/// its typed claims struct (the surface `CoseVerifier` then decodes it and the
/// session authenticator resolves the session/actor); a wrong-keyring, rotated-out,
/// revoked, or tampered token is replaced by a non-struct sentinel the verifier
/// rejects, so authentication is denied. Any other authenticator falls through to
/// the ordinary [`parse_authenticate`] path.
pub(super) fn resolve_authenticate<S: InstanceStore>(
    loaded: &Loaded<S>,
    payload: &serde_json::Value,
) -> Option<Authenticate> {
    let object = payload.as_object()?;
    let auth = object.get("auth")?.as_str()?;
    let Some(ring) = loaded.routing.cose_ring(auth) else {
        return parse_authenticate(payload, &loaded.routing);
    };
    let role = object
        .get("role")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| loaded.routing.role_for_auth(auth).map(ToOwned::to_owned))?;
    let credential = cose_gated_credential(loaded.host.engine(), ring, object.get("credential")?);
    Some(Authenticate::new(role, AuthSelection::new(auth, credential)))
}

/// Verify a cose-token credential against keyring `ring` (§17.7), yielding the
/// verified claims as the surface credential on success, or the `none` sentinel a
/// [`CoseVerifier`](auth) rejects on any verification failure.
fn cose_gated_credential<S: InstanceStore>(
    engine: &Engine<S>,
    ring: &str,
    wire: &serde_json::Value,
) -> Credential {
    match cose_token_from_wire(wire).map(|token| engine.cose_verify(ring, &token)) {
        Some(Ok(claims)) => Credential::new(claims),
        _ => Credential::new(Value::None),
    }
}

/// Reconstruct a cose-token [`Value`] from its wire JSON (the pinned §17.8 token
/// format: `$ring`/`$version`/`$claims`/`$sig`), so a login-minted token carried
/// back through the harness can be re-verified by [`Engine::cose_verify`](Engine::cose_verify).
/// Each claim decodes to its most-specific scalar so the verified `session` claim
/// matches the session row's typed key on lookup; canonical JSON is identical
/// across those scalar spellings, so the token's signed-bytes check is unaffected.
fn cose_token_from_wire(wire: &serde_json::Value) -> Option<Value> {
    let object = wire.as_object()?;
    let ring = object.get("$ring")?.as_str()?;
    let version = Type::Int.decode(object.get("$version")?).ok()?;
    let signature = Type::Bytes.decode(object.get("$sig")?).ok()?;
    let claims = object
        .get("$claims")?
        .as_object()?
        .iter()
        .map(|(name, wire)| (Text::new(name.clone()), claim_value(wire)))
        .collect::<Vec<_>>();
    Some(Value::Struct(Struct::new([
        (Text::new("$ring"), Value::Text(Text::new(ring.to_owned()))),
        (Text::new("$version"), version),
        (Text::new("$claims"), Value::Struct(Struct::new(claims))),
        (Text::new("$sig"), signature),
    ])))
}

/// Decode one claim's wire value to its most-specific scalar: a `uuid`/`int`
/// string to that typed value (so a `uuid`/`int` session-key claim matches the
/// session row's key by value), any other string to `text`, and a composite to
/// `json`.
fn claim_value(wire: &serde_json::Value) -> Value {
    match wire {
        serde_json::Value::String(text) => Type::Uuid
            .decode(wire)
            .or_else(|_| Type::Int.decode(wire))
            .unwrap_or_else(|_| Value::Text(Text::new(text.clone()))),
        serde_json::Value::Bool(flag) => Value::Bool(*flag),
        other => Json::from_wire(other).map_or_else(
            |_| Value::Text(Text::new(other.to_string())),
            Value::Json,
        ),
    }
}

/// Build a bounded window (§12.2) from a verbatim `window` spec. Handles the
/// `$size` bound with a `$first`/`$last` anchor, a concrete occurrence anchor (a
/// row key identity), and the `$slide` flag. A concrete anchor is resolved to the
/// row's stable [`RowId`] the same way the view materializer keys it (D.2): the
/// anchor's canonical key text. The engine then requires that occurrence be
/// present when the window opens (§12.2).
pub(super) fn build_window(spec: &serde_json::Value) -> Option<Window> {
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
/// same identity the view materializer assigns.
fn anchor_row_id(anchor: &str) -> liasse_expr::RowId {
    let value = Value::Text(Text::new(anchor.to_owned()));
    let text = liasse_ident::KeyText::from_key_values(std::slice::from_ref(&value))
        .map(|key| key.as_str().to_owned())
        .unwrap_or_else(|_| anchor.to_owned());
    liasse_expr::RowId::keyed(text)
}

/// Render a call outcome to a harness observation, projecting the response value
/// to canonical strict-JSON and reporting the success completion.
pub(super) fn observe_call(outcome: &liasse_surface::SurfaceOutcome) -> Observation {
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
pub(super) fn observe_subscription(subscription: &Subscription, singular: bool) -> Observation {
    match subscription {
        Subscription::Init(result) => Observation::ok(Some(wire::view_to_json_shaped(result, singular))),
        Subscription::Window(rows) => Observation::ok(Some(wire::rows_to_json(rows))),
        Subscription::Denied(_) => Observation::outcome(Outcome::Denied),
        Subscription::Failed(_) => Observation::outcome(Outcome::Error),
    }
}

/// Map a surface transport fault to a harness skip.
pub(super) fn host_fault(error: SurfaceError) -> AdapterError {
    AdapterError::Host(error.to_string())
}

/// Whether `case`'s program runs any [`operator`](StepKind::Operator) step,
/// including inside a nested `concurrently`/`in_sandbox` group.
fn case_uses_operator(case: &Case) -> bool {
    matches!(&case.body, crate::case::CaseBody::Scenario(steps) if steps_use_operator(steps))
}

/// Recursively scan `steps` (and their nested groups) for an operator step.
fn steps_use_operator(steps: &[crate::step::Step]) -> bool {
    steps.iter().any(|step| {
        matches!(step.kind, crate::step_kind::StepKind::Operator)
            || steps_use_operator(step.nested.steps())
            || step.nested.branches().iter().any(|branch| steps_use_operator(branch))
    })
}

/// Inject the synthetic [`OPERATOR_SURFACE`] into a package's `$model.$public`,
/// exposing every top-level `$mut` as a same-named call (`<mut>: ".<mut>"`). Only
/// root mutations declared at `$model.$mut` are exposed; a collection-scoped
/// mutation is not addressable this way and is left out. A package with no
/// `$model` object or no top-level `$mut` block is untouched.
fn inject_operator_surface(package: &mut serde_json::Value) {
    let Some(model) = package.get_mut("$model").and_then(serde_json::Value::as_object_mut) else {
        return;
    };
    let mut_names: Vec<String> = model
        .get("$mut")
        .and_then(serde_json::Value::as_object)
        .map(|muts| muts.keys().cloned().collect())
        .unwrap_or_default();
    if mut_names.is_empty() {
        return;
    }
    let mut calls = serde_json::Map::new();
    for name in mut_names {
        calls.insert(name.clone(), serde_json::Value::String(format!(".{name}")));
    }
    let public = model
        .entry("$public".to_owned())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(public) = public.as_object_mut() {
        public.insert(OPERATOR_SURFACE.to_owned(), serde_json::json!({ "$mut": calls }));
    }
}
