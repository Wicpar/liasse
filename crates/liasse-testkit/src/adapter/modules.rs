//! Driving the §13 module lifecycle op families over a [`ModuleDeployment`].
//!
//! The corpus's §13 cases install child module packages into the root's
//! row-scoped module spaces (`/companies/acme/modules`) and drive the lifecycle
//! verbs — `module_install`/`module_disable`/`module_enable`/`module_uninstall`/
//! `module_rename`/`module_update` — against them. This module builds a
//! [`ModuleDeployment`] over the case's root package and drives those verbs into
//! it, mapping each [`ModuleObservation`]/[`ModuleUpdate`] to the harness outcome
//! vocabulary: §13.3's static name rule (`EmptyName`) is an `invalid`, and its
//! admission rules (`DuplicateName`, an unknown/disabled instance) are `rejected`
//! (see `tests/13-modules/NOTES.md`).
//!
//! # What routes end to end here, and the seams it does not reach
//!
//! The deployment's root engine is a **separate** engine from the base
//! [`SurfaceHost`](liasse_surface::SurfaceHost): the runtime keeps installed
//! children external to the root engine (they live inside the
//! [`ModuleHost`](liasse_runtime::ModuleHost), not in the root's own committed
//! state), so the base host's `$public` surfaces cannot observe them. The
//! adapter therefore *routes a root read or call that addresses `.modules`
//! through the deployment* rather than the base host: a `watch`/`expect_view` on a
//! surface whose view aggregates `.modules::iface` (§13.9) evaluates through
//! [`ModuleDeployment::root_view`], which folds the enabled children into the read
//! ([`ModuleState::root_view`]); a `call` on a surface whose `$mut` is a
//! `::`-interface reference (§13.10) — one the base router leaves unbound, so it
//! would resolve `denied` — dispatches through [`ModuleDeployment::interface_call`]
//! ([`ModuleState::interface_call`]). The installation `$data` overlay (§13.3) is
//! now recorded on the [`InstallRequest`], so an overlay row failing a `$check` is
//! an admission `rejected`.
//!
//! The remaining seams stay blocked and recorded per case in `scenario_gate`:
//! `$config` type-checking (§13.1), peer/`$deps` resolution (§13.5/§13.6), the
//! interface-contract satisfaction check at install (§13.8), and the §13.15
//! update-report assembly — all runtime/surface work the current
//! [`ModuleDeployment`] does not yet close. A parent surface that mixes a base-host
//! root mutation with a `.modules` aggregation would also need the two engines
//! reconciled; no §13 case does, since every §13 mutation is an install or an
//! interface call, both routed to the deployment.

use std::collections::{BTreeMap, BTreeSet};

use liasse_diag::SourceMap;
use liasse_ident::InstanceId;
use liasse_runtime::{
    CallOutcome, CallRequest, Engine, InstallRequest, ModuleError, ModuleHost, ModuleSpace,
    Precision, ViewQuery,
};
use liasse_store::{InstanceStore, MemoryStore, MemoryStoreFactory};
use liasse_surface::{
    Entropy, ModuleDeployment, ModuleFault, ModuleObservation, ModuleUpdate,
    VirtualClock as SurfaceClock,
};
use liasse_syntax::{parse_expression, Expr, ExprKind, Selector, StmtKind};
use liasse_value::{Json, Text, Type, Value};

use crate::contract::Observation;
use crate::outcome::{Completion, Outcome};

use super::{wire, AdapterError, EPOCH_MICROS};

/// A recorded module-routed subscription (§13.9): the root surface view address
/// and its arguments, replayed by a later `expect_view` so the module-aware read
/// re-evaluates against the deployment's current state (a disable/enable between
/// the `watch` and the `expect_view` changes what the aggregation observes).
#[derive(Debug, Clone)]
pub(super) struct ModuleWatch {
    /// The surface view address (`public.<surface>`) the watch subscribed.
    pub(super) address: String,
    /// The subscription's view arguments, verbatim.
    pub(super) args: serde_json::Value,
    /// Whether the bound view delivers a single object (§12.2).
    pub(super) singular: bool,
}

/// The live §13 module deployment for one case: a root engine plus the child
/// instances installed into its module spaces, together with the case's package
/// map so an install/update can resolve a `$module` line to its child definition.
pub(super) struct ModuleState {
    deployment: ModuleDeployment<MemoryStoreFactory>,
    /// Label → raw child package definition, resolved by each entry's `$module`.
    packages: serde_json::Map<String, serde_json::Value>,
    /// Interface-addressed surface `$mut` bindings (§13.10), keyed by call address
    /// (`public.<surface>.<call>`): the base surface router cannot bind a
    /// `::`-interface reference (`/companies["acme"].modules["kit"]::templates.create`),
    /// so a `call` on such a surface routes here to [`ModuleDeployment::interface_call`]
    /// rather than resolving `denied` on the base host.
    interface_calls: BTreeMap<String, InterfaceRef>,
}

impl ModuleState {
    /// Build a deployment over the case's already-prepared root `definition`. The
    /// root engine and every installed child run over the in-memory store factory
    /// regardless of the base backend, so the module verdicts are identical across
    /// backends (a store-contract divergence would be a bug, not a design choice).
    pub(super) fn build(
        instance: &str,
        definition: &str,
        packages: &serde_json::Map<String, serde_json::Value>,
        package: &serde_json::Value,
    ) -> Result<Self, String> {
        let store = MemoryStore::new(InstanceId::new(format!("{instance}#modroot")));
        let mut clock = SurfaceClock::new(EPOCH_MICROS, Precision::Micros);
        let root = Engine::load(store, definition, &mut clock).map_err(|err| err.to_string())?;
        // §5.1/§8.12: production module admission draws generated `uuid()` seeds from
        // the OS CSPRNG (unpredictable module tokens). The corpus matches generated
        // values reproducibly, so the harness pins the SAME injectable seam a real
        // deployment uses to a DETERMINISTIC CSPRNG source, seeded from the root's
        // post-genesis counter — reproducible run-to-run while still exercising the
        // CSPRNG path rather than the predictable clock counter.
        let seed = clock.seed();
        let host = ModuleHost::new(MemoryStoreFactory::new(), root);
        Ok(Self {
            deployment: ModuleDeployment::new(host, clock).with_entropy(Entropy::seeded(seed)),
            packages: packages.clone(),
            interface_calls: interface_call_bindings(package),
        })
    }

    /// §13.3 `modules.install`: resolve the child `$module` package, build an
    /// [`InstallRequest`] from the step's `request` block (`$name`/`$module`/
    /// `$config`/`$use`, plus the child package's declared `$use`/`$deps`), and
    /// admit it into the named module space.
    pub(super) fn install(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let space = self.space(target)?;
        let Some(request) = target.get("request").and_then(serde_json::Value::as_object) else {
            return Err(AdapterError::unsupported("`module_install` step carries no `request` block"));
        };
        let name = request.get("$name").and_then(serde_json::Value::as_str).unwrap_or_default();
        let Some(module) = request.get("$module").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`module_install` request names no `$module`"));
        };
        let package = self.child_package(module)?;
        let definition =
            serde_json::to_string(&package).map_err(|err| AdapterError::Host(err.to_string()))?;
        let mut install = InstallRequest::new(name.to_owned(), definition);
        // §13.1/§13.5/§13.6: the immutable `$config` values and the explicit `$use`
        // bindings the operator supplies in the request, plus the child package's
        // own declared `$use`/`$deps` boundary requirements.
        install = record_uses(install, request.get("$use"));
        install = record_uses(install, package.get("$use"));
        install = record_deps(install, package.get("$deps"));
        install = record_config(install, request.get("$config"));
        // §13.3: the installation `$data` overlays onto the child genesis after the
        // package `$data` seed; every resulting value passes ordinary insertion and
        // load validation, so a row whose field fails a `$check` refuses the install.
        install = record_data(install, request.get("$data"));
        match self.deployment.install(&space, install) {
            Ok(observation) => Ok(observe(observation)),
            Err(fault) => Ok(Observation::outcome(install_fault_outcome(&fault))),
        }
    }

    /// §13.3/§13.12 `modules.disable`.
    pub(super) fn disable(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let (space, name) = self.instance(target)?;
        match self.deployment.disable(&space, &name) {
            Ok(observation) => Ok(observe(observation)),
            Err(fault) => Err(AdapterError::Host(format!("module disable fault: {fault}"))),
        }
    }

    /// §13.3 `modules.enable`.
    pub(super) fn enable(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let (space, name) = self.instance(target)?;
        match self.deployment.enable(&space, &name) {
            Ok(observation) => Ok(observe(observation)),
            Err(fault) => Err(AdapterError::Host(format!("module enable fault: {fault}"))),
        }
    }

    /// §13.3/§13.12 `modules.uninstall`.
    pub(super) fn uninstall(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let (space, name) = self.instance(target)?;
        match self.deployment.uninstall(&space, &name) {
            Ok(observation) => Ok(observe(observation)),
            Err(fault) => Err(AdapterError::Host(format!("module uninstall fault: {fault}"))),
        }
    }

    /// §13.3 `modules.rename`: a rekey preserving the incarnation (D.1).
    pub(super) fn rename(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let (space, name) = self.instance(target)?;
        let Some(to) = target.get("to").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`module_rename` step names no `to` instance name"));
        };
        match self.deployment.rename(&space, &name, to) {
            Ok(observation) => Ok(observe(observation)),
            Err(fault) => Err(AdapterError::Host(format!("module rename fault: {fault}"))),
        }
    }

    /// §13.14 `modules.update`: migrate a single instance to the `to` package line.
    pub(super) fn update(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let (space, name) = self.instance(target)?;
        let Some(to) = target.get("to").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`module_update` step names no `to` package line"));
        };
        let package = self.child_package(to)?;
        let definition =
            serde_json::to_string(&package).map_err(|err| AdapterError::Host(err.to_string()))?;
        match self.deployment.update(&space, &name, &definition) {
            // §13.15: the update-report shape ($instance/$from/$to/$migrated/
            // $seeded/$exposed/$imports/$commit) is not assembled by the runtime
            // host — it returns a §20 migration report — so a successful update
            // carries no value here (assembling the §13.15 report is a runtime seam).
            Ok(ModuleUpdate::Updated(_)) => Ok(Observation::ok(None)),
            Ok(ModuleUpdate::Unknown(_) | ModuleUpdate::Disabled(_)) => {
                Ok(Observation::outcome(Outcome::Rejected))
            }
            // §13.14: a narrowing/rejected migration is collapsed into an engine
            // fault by the runtime host; distinguishing the §13.14 refusal from a
            // genuine store fault (and classifying it `invalid`) is a runtime seam.
            Err(fault) => Err(AdapterError::unsupported(format!(
                "`module_update` migration was refused and collapsed into an engine fault by the \
                 runtime host — surfacing the §13.14/§13.15 outcome is a runtime seam: {fault}"
            ))),
        }
    }

    /// Evaluate a root package surface view that reads its installed children
    /// through `.modules::iface` (§13.9), folding the enabled instances into the
    /// read via [`ModuleDeployment::root_view`]. This is the entry a `watch`/
    /// `expect_view` on a `.modules`-aggregating root surface routes through: the
    /// base surface host reads the root engine alone, which cannot observe the
    /// children installed in the deployment (and faults on a `.modules::` read with
    /// no module data), so the aggregation is served here instead. `None` when the
    /// deployment declares no surface view of that name — the caller then falls back
    /// to the base host.
    pub(super) fn root_view(
        &self,
        address: &str,
        args: &serde_json::Value,
        singular: bool,
    ) -> Option<Observation> {
        let types: BTreeMap<String, Type> =
            self.deployment.root().surface_view_params(address).into_iter().collect();
        // §12.1 step 3 / Annex A.1: a `$params` argument that does not decode
        // against its declared type is a malformed request, rejected rather than
        // coerced to a best-effort inference.
        let Ok(decoded) = wire::decode_args(args, &types) else {
            return Some(Observation::outcome(Outcome::Rejected));
        };
        let mut query = ViewQuery::new();
        for (name, value) in decoded {
            query = query.param(name, value);
        }
        match self.deployment.root_view(address, &query) {
            Ok(Some(result)) => Some(Observation::ok(Some(wire::view_to_json_shaped(&result, singular)))),
            _ => None,
        }
    }

    /// The interface-addressed binding of the surface call at `address`
    /// (`public.<surface>.<call>`), if the surface routes to a child's `$expose`d
    /// mutation through a `::`-interface reference (§13.10).
    pub(super) fn interface_ref(&self, address: &str) -> Option<&InterfaceRef> {
        self.interface_calls.get(address)
    }

    /// Dispatch an interface-addressed call to a child's `$expose`d mutation
    /// (§13.10): resolve the module space and instance from the call `args`, forward
    /// the child mutation's own arguments, and admit it against the enabled child.
    pub(super) fn interface_call(
        &mut self,
        iface: &InterfaceRef,
        args: &serde_json::Value,
    ) -> Result<Observation, AdapterError> {
        let Some(resolved) = iface.resolve(args) else {
            return Err(AdapterError::unsupported(
                "an interface-addressed call could not resolve its module space/instance from the \
                 call arguments",
            ));
        };
        // §13.10: the child mutation receives every argument the selector did not
        // consume (the space/instance `@param`s address the instance, not the child).
        let forwarded = forward_args(args, &resolved.consumed);
        // §12.1 step 3 / Annex A.1: a forwarded child-mutation argument that does
        // not decode is a malformed request, rejected rather than coerced. The
        // forwarded arguments carry no resolved types here, so each is shape-
        // inferred (§8.3) and this decode does not fail in practice.
        let Ok(forwarded_args) = wire::decode_args(&forwarded, &BTreeMap::new()) else {
            return Ok(Observation::outcome(Outcome::Rejected));
        };
        let mut request = CallRequest::new(String::new());
        for (name, value) in forwarded_args {
            request = request.arg(name, value);
        }
        match self.deployment.interface_call(
            &resolved.space,
            &resolved.instance,
            &resolved.interface,
            &resolved.mutation,
            &request,
        ) {
            Ok(outcome) => Ok(observe_call_outcome(&outcome)),
            // §13.3/§13.12: an absent/disabled instance, or an interface that binds
            // no such routable mutation, refuses the addressed transition — an
            // admission `rejected`, not a store fault.
            Err(ModuleError::Unknown(_) | ModuleError::Disabled(_) | ModuleError::InterfaceContract(..)) => {
                Ok(Observation::outcome(Outcome::Rejected))
            }
            Err(fault) => Err(AdapterError::Host(format!("interface call fault: {fault}"))),
        }
    }

    /// The module space the `space` member of an install step names (§13.2).
    fn space(&self, target: &serde_json::Value) -> Result<ModuleSpace, AdapterError> {
        let Some(path) = target.get("space").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`module_install` step names no `space`"));
        };
        ModuleSpace::new(path).map_err(|_| AdapterError::Host(format!("malformed module space `{path}`")))
    }

    /// The `(space, instance name)` a lifecycle step's `instance` display path names
    /// (§13.3): the trailing component is the instance name, the prefix its space.
    fn instance(&self, target: &serde_json::Value) -> Result<(ModuleSpace, String), AdapterError> {
        let Some(path) = target.get("instance").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("module lifecycle step names no `instance` path"));
        };
        let Some((space, name)) = split_instance(path) else {
            return Err(AdapterError::Host(format!("malformed instance path `{path}`")));
        };
        let space = ModuleSpace::new(space)
            .map_err(|_| AdapterError::Host(format!("malformed module space in `{path}`")))?;
        Ok((space, name.to_owned()))
    }

    /// The child package whose declared `$module` line is `module`.
    fn child_package(&self, module: &str) -> Result<serde_json::Value, AdapterError> {
        self.packages
            .values()
            .find(|package| package.get("$module").and_then(serde_json::Value::as_str) == Some(module))
            .cloned()
            .ok_or_else(|| {
                AdapterError::unsupported(format!(
                    "no package in the case's packages map declares `$module: {module}`"
                ))
            })
    }
}

/// Map a §13.3 lifecycle observation to the harness outcome vocabulary. `EmptyName`
/// and a malformed binding are static-validation failures (`invalid`); a duplicate
/// name, unknown/disabled instance, or malformed space is an admission `rejected`.
fn observe(observation: ModuleObservation) -> Observation {
    match observation {
        ModuleObservation::Applied => Observation::ok(None),
        ModuleObservation::EmptyName | ModuleObservation::InvalidBinding(_) => {
            Observation::outcome(Outcome::Invalid)
        }
        ModuleObservation::DuplicateName(_)
        | ModuleObservation::Unknown(_)
        | ModuleObservation::Disabled(_)
        | ModuleObservation::InvalidSpace(_)
        | ModuleObservation::MissingContainingRow(_)
        // §13.5: an unresolvable required peer binding is an admission-time refusal.
        | ModuleObservation::PeerUnresolved(_) => Observation::outcome(Outcome::Rejected),
    }
}

/// Split a module instance display path into `(space, name)`: the trailing path
/// component is the instance name, the prefix is its module space.
fn split_instance(path: &str) -> Option<(&str, &str)> {
    let (space, name) = path.rsplit_once('/')?;
    (!space.is_empty() && !name.is_empty()).then_some((space, name))
}

/// Record a `$use` object's handles onto an install request (§13.5), including the
/// `$optional` group whose absence is valid. Shared by the request-supplied `$use`
/// and the child package's own declared `$use`.
fn record_uses(mut install: InstallRequest, uses: Option<&serde_json::Value>) -> InstallRequest {
    let Some(map) = uses.and_then(serde_json::Value::as_object) else {
        return install;
    };
    for (handle, spec) in map {
        if handle == "$optional" {
            if let Some(optional) = spec.as_object() {
                for (optional_handle, optional_spec) in optional {
                    if let Some(optional_spec) = optional_spec.as_str() {
                        install = install.optional_use(optional_handle.clone(), optional_spec);
                    }
                }
            }
            continue;
        }
        if let Some(spec) = spec.as_str() {
            install = install.use_handle(handle.clone(), spec);
        }
    }
    install
}

/// Record a `$deps` object's private requirements onto an install request (§13.6).
fn record_deps(mut install: InstallRequest, deps: Option<&serde_json::Value>) -> InstallRequest {
    let Some(map) = deps.and_then(serde_json::Value::as_object) else {
        return install;
    };
    for (handle, spec) in map {
        if let Some(spec) = spec.as_str() {
            install = install.dep(handle.clone(), spec);
        }
    }
    install
}

/// Record the immutable `$config` installation values onto an install request
/// (§13.1). Each value decodes to its most-specific scalar; the runtime records
/// them and type-checking against the declared `$config` struct is a runtime seam.
fn record_config(mut install: InstallRequest, config: Option<&serde_json::Value>) -> InstallRequest {
    let Some(fields) = config.and_then(serde_json::Value::as_object) else {
        return install;
    };
    for (field, wire) in fields {
        install = install.config(field.clone(), decode_config_value(wire));
    }
    install
}

/// Decode a `$config` wire value to a runtime [`Value`]: a string to `text`, a
/// boolean to `bool`, and any composite to `json`.
fn decode_config_value(wire: &serde_json::Value) -> Value {
    match wire {
        serde_json::Value::String(text) => Value::Text(Text::new(text.clone())),
        serde_json::Value::Bool(flag) => Value::Bool(*flag),
        other => Json::from_wire(other)
            .map_or_else(|_| Value::Text(Text::new(other.to_string())), Value::Json),
    }
}

/// Record the installation `$data` overlay onto an install request (§13.3), as the
/// JSON text of the `$data` object. Absent or non-serializable `$data` is left off.
fn record_data(install: InstallRequest, data: Option<&serde_json::Value>) -> InstallRequest {
    match data.and_then(|data| serde_json::to_string(data).ok()) {
        Some(text) => install.data(text),
        None => install,
    }
}

/// Classify a module install fault the surface collapses into a [`ModuleFault`]
/// (§13.3). `ModuleFault` erases the distinct §13.3 failure classes, exposing only
/// its diagnostic text: a seed/overlay admission refusal (an installation `$data`
/// row failing ordinary insertion validation) is an admission `rejected`, while a
/// static definition/`$config`/interface-contract validation failure is `invalid`
/// (the FORMAT.md build/load-vs-admission split, tests/13-modules/NOTES.md).
/// Reading the class off the fault text is a surface seam — `ModuleFault` should
/// carry the classified outcome but does not expose the inner error.
fn install_fault_outcome(fault: &ModuleFault) -> Outcome {
    if fault.to_string().contains("seed rejected") {
        Outcome::Rejected
    } else {
        Outcome::Invalid
    }
}

/// Render a child mutation [`CallOutcome`] to a harness observation (§13.10): a
/// committed/unchanged transition carries its `$return` projection (`None` for a
/// response-free mutation, §13.8), a rejected transition its outcome class.
fn observe_call_outcome(outcome: &CallOutcome) -> Observation {
    match outcome {
        CallOutcome::Committed { response, .. } => Observation {
            outcome: Outcome::Ok,
            value: response.as_ref().map(wire::response_to_json),
            completion: Some(Completion::Committed),
            extra: serde_json::Map::new(),
        },
        CallOutcome::Unchanged { response } => Observation {
            outcome: Outcome::Ok,
            value: response.as_ref().map(wire::response_to_json),
            completion: Some(Completion::Unchanged),
            extra: serde_json::Map::new(),
        },
        CallOutcome::Rejected(_) => Observation::outcome(Outcome::Rejected),
    }
}

/// The call `args` restricted to the members the space/instance selectors did not
/// consume — the arguments forwarded to the child mutation (§13.10).
fn forward_args(args: &serde_json::Value, consumed: &BTreeSet<String>) -> serde_json::Value {
    let Some(map) = args.as_object() else {
        return serde_json::Value::Object(serde_json::Map::new());
    };
    let forwarded: serde_json::Map<String, serde_json::Value> =
        map.iter().filter(|(name, _)| !consumed.contains(*name)).map(|(k, v)| (k.clone(), v.clone())).collect();
    serde_json::Value::Object(forwarded)
}

/// The interface-addressed surface `$mut` bindings of a root package, keyed by call
/// address (`public.<surface>.<call>` / `<role>.<surface>.<call>`): each `$mut`
/// value that is a `::`-interface reference the base surface router cannot bind.
fn interface_call_bindings(package: &serde_json::Value) -> BTreeMap<String, InterfaceRef> {
    let mut map = BTreeMap::new();
    let Some(model) = package.get("$model").and_then(serde_json::Value::as_object) else {
        return map;
    };
    if let Some(public) = model.get("$public").and_then(serde_json::Value::as_object) {
        collect_surface_interface_calls("public", public, &mut map);
    }
    if let Some(roles) = model.get("$roles").and_then(serde_json::Value::as_object) {
        for (role, definition) in roles {
            if let Some(surfaces) = definition.as_object() {
                collect_surface_interface_calls(role, surfaces, &mut map);
            }
        }
    }
    map
}

/// Record each surface's interface-addressed `$mut` calls under `prefix`.
fn collect_surface_interface_calls(
    prefix: &str,
    surfaces: &serde_json::Map<String, serde_json::Value>,
    map: &mut BTreeMap<String, InterfaceRef>,
) {
    for (surface, definition) in surfaces {
        if surface.starts_with('$') {
            continue;
        }
        let Some(calls) = definition.get("$mut").and_then(serde_json::Value::as_object) else {
            continue;
        };
        for (call, body) in calls {
            if let Some(iface) = body.as_str().and_then(InterfaceRef::parse) {
                map.insert(format!("{prefix}.{surface}.{call}"), iface);
            }
        }
    }
}

/// A component of an interface-addressed reference's module-space/instance path:
/// either a literal key or a `@param` resolved from the call arguments.
#[derive(Debug, Clone)]
enum PathSeg {
    Lit(String),
    Param(String),
}

/// A parsed interface-addressed surface `$mut` reference (§13.10), e.g.
/// `/companies[@company].modules[@module]::templates.create`: the module-space path
/// template, the instance-name selector, the interface, and the routed mutation.
#[derive(Debug, Clone)]
pub(super) struct InterfaceRef {
    space: Vec<PathSeg>,
    instance: PathSeg,
    interface: String,
    mutation: String,
}

/// An [`InterfaceRef`] resolved against a call's arguments.
struct ResolvedInterfaceCall {
    space: ModuleSpace,
    instance: String,
    interface: String,
    mutation: String,
    /// The argument names the space/instance selectors consumed.
    consumed: BTreeSet<String>,
}

impl InterfaceRef {
    /// Parse a surface `$mut` reference into an interface-addressed binding, or
    /// `None` when it is not a `[<instance>]::<interface>.<mutation>` reference the
    /// base surface router already binds (a plain receiver-and-parameters call).
    fn parse(text: &str) -> Option<Self> {
        let mut sources = SourceMap::new();
        let source = sources.add_label("iface-ref", text.to_owned());
        let parsed = parse_expression(source, text).ok()?;
        let StmtKind::Bare(expr) = &parsed.statement().kind else {
            return None;
        };
        // A bare `.…::iface.mut` reference, or an explicit `.…::iface.mut()` call.
        let field = match &expr.kind {
            ExprKind::Field { .. } => expr,
            ExprKind::Call { callee, args } if args.is_empty() => callee.as_ref(),
            _ => return None,
        };
        let ExprKind::Field { base, member: mutation } = &field.kind else {
            return None;
        };
        let ExprKind::SameName { base: selected, member: interface } = &base.kind else {
            return None;
        };
        let ExprKind::Select { base: space_expr, selector: Selector::Keys(keys) } = &selected.kind else {
            return None;
        };
        let [instance_key] = keys.as_slice() else {
            return None;
        };
        Some(Self {
            space: walk_space(space_expr)?,
            instance: key_seg(instance_key)?,
            interface: interface.text.clone(),
            mutation: mutation.text.clone(),
        })
    }

    /// Resolve the module space and instance name against the call `args`.
    fn resolve(&self, args: &serde_json::Value) -> Option<ResolvedInterfaceCall> {
        let mut consumed = BTreeSet::new();
        let mut path = String::new();
        for seg in &self.space {
            path.push('/');
            path.push_str(&resolve_seg(seg, args, &mut consumed)?);
        }
        let instance = resolve_seg(&self.instance, args, &mut consumed)?;
        let space = ModuleSpace::new(&path).ok()?;
        Some(ResolvedInterfaceCall {
            space,
            instance,
            interface: self.interface.clone(),
            mutation: self.mutation.clone(),
            consumed,
        })
    }
}

/// The path segment a selector key expression names: a string literal or a
/// `@param`. A computed or non-scalar key is unsupported here.
fn key_seg(expr: &Expr) -> Option<PathSeg> {
    match &expr.kind {
        ExprKind::Str(text) => Some(PathSeg::Lit(text.clone())),
        ExprKind::Param(id) => Some(PathSeg::Param(id.text.clone())),
        _ => None,
    }
}

/// Walk a module-space reference expression (`/companies["acme"].modules`) into its
/// display-path segments, in order. Each field access is a literal component and
/// each key selector a literal or `@param` component.
fn walk_space(expr: &Expr) -> Option<Vec<PathSeg>> {
    match &expr.kind {
        ExprKind::Root | ExprKind::Current => Some(Vec::new()),
        ExprKind::Field { base, member } => {
            let mut segs = walk_space(base)?;
            segs.push(PathSeg::Lit(member.text.clone()));
            Some(segs)
        }
        ExprKind::Select { base, selector: Selector::Keys(keys) } => {
            let [key] = keys.as_slice() else {
                return None;
            };
            let mut segs = walk_space(base)?;
            segs.push(key_seg(key)?);
            Some(segs)
        }
        _ => None,
    }
}

/// Resolve one path segment to its display-path component, recording a consumed
/// `@param`. A `@param` resolves to its `text` argument.
fn resolve_seg(seg: &PathSeg, args: &serde_json::Value, consumed: &mut BTreeSet<String>) -> Option<String> {
    match seg {
        PathSeg::Lit(text) => Some(text.clone()),
        PathSeg::Param(name) => {
            consumed.insert(name.clone());
            args.get(name).and_then(serde_json::Value::as_str).map(ToOwned::to_owned)
        }
    }
}

impl<S: InstanceStore> super::ScenarioAdapter<S> {
    /// The case's live module deployment, built lazily on first module op from the
    /// case's prepared root definition and its package map. A build failure (the
    /// root package did not load into a fresh engine) is cached and surfaces as a
    /// skip on every module op.
    pub(super) fn module_state(&mut self) -> Result<&mut ModuleState, AdapterError> {
        if self.module.is_none() {
            let plan = super::auth::AuthPlan::derive(&self.load_ctx.package, self.load_ctx.hosts.as_ref());
            let built = match super::prepared_definition(&self.load_ctx.package, &plan, &self.load_ctx.lift) {
                Some(definition) => ModuleState::build(
                    self.load_ctx.instance.as_str(),
                    &definition,
                    &self.packages,
                    &self.load_ctx.package,
                ),
                None => Err("prepared root definition did not serialize".to_owned()),
            };
            self.module = Some(built);
        }
        match self.module.as_mut() {
            Some(Ok(state)) => Ok(state),
            Some(Err(reason)) => Err(AdapterError::unsupported(format!(
                "module deployment could not be built for this case: {reason}"
            ))),
            None => Err(AdapterError::unsupported("module deployment not initialised")),
        }
    }
}
