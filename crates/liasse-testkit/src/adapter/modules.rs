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
//! state), so the root's own `$public` surfaces and `.modules::iface` views
//! cannot observe them. Every §13 case that reads installed-module data back
//! *through a parent surface* — the `.modules::templates` aggregation, a
//! `.modules[name]::iface` interface read, a `.modules[name]::iface.mut`
//! interface mutation — therefore sees an empty module space on the base host and
//! stays blocked on that surface/runtime integration seam. The install request's
//! `$data` overlay (§13.3), `$config` type-checking (§13.1), peer/`$deps`
//! resolution (§13.5/§13.6), and the §13.15 update-report assembly are further
//! runtime seams the current [`ModuleDeployment`] does not yet close. Each is
//! recorded per case in `scenario_gate`. The lifecycle *outcomes* (name
//! validation, duplicate detection, disable/enable/uninstall/rename admission)
//! route end to end.

use liasse_ident::InstanceId;
use liasse_runtime::{Engine, InstallRequest, ModuleHost, ModuleSpace, Precision};
use liasse_store::{InstanceStore, MemoryStore, MemoryStoreFactory};
use liasse_surface::{ModuleDeployment, ModuleObservation, ModuleUpdate, VirtualClock as SurfaceClock};
use liasse_value::{Json, Text, Value};

use crate::contract::Observation;
use crate::outcome::Outcome;

use super::{AdapterError, EPOCH_MICROS};

/// The live §13 module deployment for one case: a root engine plus the child
/// instances installed into its module spaces, together with the case's package
/// map so an install/update can resolve a `$module` line to its child definition.
pub(super) struct ModuleState {
    deployment: ModuleDeployment<MemoryStoreFactory>,
    /// Label → raw child package definition, resolved by each entry's `$module`.
    packages: serde_json::Map<String, serde_json::Value>,
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
    ) -> Result<Self, String> {
        let store = MemoryStore::new(InstanceId::new(format!("{instance}#modroot")));
        let mut clock = SurfaceClock::new(EPOCH_MICROS, Precision::Micros);
        let root = Engine::load(store, definition, &mut clock).map_err(|err| err.to_string())?;
        let host = ModuleHost::new(MemoryStoreFactory::new(), root);
        Ok(Self { deployment: ModuleDeployment::new(host, clock), packages: packages.clone() })
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
        match self.deployment.install(&space, install) {
            Ok(observation) => Ok(observe(observation)),
            // §13.3: "Loading validates the definition, the configuration, and the
            // interface contract before the instance becomes active." A child whose
            // definition fails static validation is refused before it becomes
            // active — a static `invalid`.
            Err(_) => Ok(Observation::outcome(Outcome::Invalid)),
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
        | ModuleObservation::InvalidSpace(_) => Observation::outcome(Outcome::Rejected),
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

impl<S: InstanceStore> super::ScenarioAdapter<S> {
    /// The case's live module deployment, built lazily on first module op from the
    /// case's prepared root definition and its package map. A build failure (the
    /// root package did not load into a fresh engine) is cached and surfaces as a
    /// skip on every module op.
    pub(super) fn module_state(&mut self) -> Result<&mut ModuleState, AdapterError> {
        if self.module.is_none() {
            let plan = super::auth::AuthPlan::derive(&self.load_ctx.package, self.load_ctx.hosts.as_ref());
            let built = match super::prepared_definition(&self.load_ctx.package, &plan, &self.load_ctx.lift) {
                Some(definition) => {
                    ModuleState::build(self.load_ctx.instance.as_str(), &definition, &self.packages)
                }
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
