//! Driving the §13 module lifecycle op families over a [`ModuleDeployment`].
//!
//! The corpus's §13 cases install child module packages into the root's
//! row-scoped module collections (`/companies/acme/modules`) and drive the lifecycle
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

use liasse_artifact::ArtifactBuilder;
use liasse_diag::SourceMap;
use liasse_ident::{HistoryPoint, InstanceId, LineageId, PointId};
use liasse_runtime::{
    CallOutcome, CallRequest, Engine, InstallRequest, ModuleError, ModuleHost,
    Precision, ViewQuery,
};
use liasse_store::{CollectionPath, InstanceStore, KeyValue, MemoryStore, MemoryStoreFactory, RowAddress};
use liasse_surface::{
    Entropy, ModuleDeployment, ModuleFault, ModuleObservation, ModuleUpdate, ModuleUpdatePreview,
    ModuleUpdateReport, VirtualClock as SurfaceClock,
};
use liasse_syntax::{parse_expression, Expr, ExprKind, Selector, StmtKind};
use liasse_value::{BlobDescriptor, Json, Text, Type, Value};

use crate::contract::Observation;
use crate::outcome::{Completion, Outcome};

use super::namespaces::parse_type;
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
/// instances installed into its module collections, together with the case's package
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
    /// The root package's declared `$mut` parameter types (§8.3), keyed by mutation
    /// name — what a `module_lifecycle_call` decodes its arguments against, since a
    /// host-scope lifecycle call names a root mutation directly rather than a
    /// surface the router typed.
    mutation_params: BTreeMap<String, BTreeMap<String, Type>>,
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
            mutation_params: root_mutation_params(package),
        })
    }

    /// §13.3 `modules.install`: resolve the child `$module` package, build an
    /// [`InstallRequest`] from the step's `request` block (`$name`/`$module`/
    /// `$config`/`$use`, plus the child package's declared `$use`/`$deps`), and
    /// admit it into the named module collection.
    pub(super) fn install(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let collection = self.collection(target)?;
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
        match self.deployment.install(&collection, install) {
            Ok(observation) => Ok(observe(observation)),
            Err(fault) => Ok(Observation::outcome(install_fault_outcome(&fault))),
        }
    }

    /// §13.3/§13.12 `modules.disable`.
    pub(super) fn disable(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let at = self.instance(target)?;
        match self.deployment.disable(&at) {
            Ok(observation) => Ok(observe(observation)),
            Err(fault) => Err(AdapterError::Host(format!("module disable fault: {fault}"))),
        }
    }

    /// §13.3 `modules.enable`.
    pub(super) fn enable(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let at = self.instance(target)?;
        match self.deployment.enable(&at) {
            Ok(observation) => Ok(observe(observation)),
            Err(fault) => Err(AdapterError::Host(format!("module enable fault: {fault}"))),
        }
    }

    /// §13.3/§13.12 `modules.uninstall`.
    pub(super) fn uninstall(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let at = self.instance(target)?;
        match self.deployment.uninstall(&at) {
            Ok(observation) => Ok(observe(observation)),
            Err(fault) => Err(AdapterError::Host(format!("module uninstall fault: {fault}"))),
        }
    }

    /// §13.3 `modules.rename`: a rekey preserving the incarnation (D.1).
    pub(super) fn rename(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let at = self.instance(target)?;
        let Some(to) = target.get("to").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`module_rename` step names no `to` instance name"));
        };
        match self.deployment.rename(&at, to) {
            Ok(observation) => Ok(observe(observation)),
            Err(fault) => Err(AdapterError::Host(format!("module rename fault: {fault}"))),
        }
    }

    /// §13.14/§13.15 `modules.update`: migrate a single instance to the `to` package
    /// line, mapping the deployment's [`ModuleUpdate`] to the harness outcome. A
    /// successful update carries the assembled §13.15 report value; a §13.14
    /// exposed-surface narrowing is refused before admission — a definitional
    /// self-narrowing by package loading (`invalid`) or a withdrawn interface binding
    /// (`rejected`) — leaving the current release active (E.9).
    pub(super) fn update(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let at = self.instance(target)?;
        let instance = target.get("instance").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
        let definition = self.update_definition(target)?;
        match self.deployment.update(&at, &definition) {
            // §13.15: assemble the update-report shape, adding the instance display
            // path the driver knows.
            Ok(ModuleUpdate::Updated(report)) => Ok(Observation::ok(Some(update_report_value(&instance, &report)))),
            // §13.14: a definitional exposed-surface narrowing is refused by package
            // loading before admission — the FORMAT.md `invalid` (tests/13-modules/NOTES.md).
            Ok(ModuleUpdate::Narrowed(_)) => Ok(Observation::outcome(Outcome::Invalid)),
            // §13.14/§13.3: a withdrawn interface binding is an admission recheck
            // refusal; an unknown/disabled instance likewise refuses the update.
            Ok(ModuleUpdate::Rejected(_) | ModuleUpdate::Unknown(_) | ModuleUpdate::Disabled(_)) => {
                Ok(Observation::outcome(Outcome::Rejected))
            }
            Err(fault) => Err(AdapterError::Host(format!("module update fault: {fault}"))),
        }
    }

    /// §20.4 `modules.update` with `dry_run: true`: the very same §13.14/§13.15
    /// computation, its prepared plan discarded. Nothing migrates, so a later step
    /// still observes the release the instance was already running — and the
    /// reported outcome is mapped from the same refusal classes
    /// [`update`](Self::update) maps, because the same computation produced them.
    pub(super) fn dry_run_update(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let at = self.instance(target)?;
        let definition = self.update_definition(target)?;
        match self.deployment.dry_run_update(&at, &definition) {
            // A dry run takes no commit, so it reports the outcome alone: the
            // §13.15 report is what the effecting update returns.
            Ok(ModuleUpdatePreview::Ready(_)) => Ok(Observation::ok(None)),
            Ok(ModuleUpdatePreview::Narrowed(_)) => Ok(Observation::outcome(Outcome::Invalid)),
            Ok(
                ModuleUpdatePreview::Rejected(_)
                | ModuleUpdatePreview::Unknown(_)
                | ModuleUpdatePreview::Disabled(_),
            ) => Ok(Observation::outcome(Outcome::Rejected)),
            Err(fault) => Err(AdapterError::Host(format!("module dry-run update fault: {fault}"))),
        }
    }

    /// The child definition the step's `to` package line names — resolved
    /// identically for an effecting `module_update` and its §20.4 dry run, so the
    /// two run over the same target bytes.
    fn update_definition(&self, target: &serde_json::Value) -> Result<String, AdapterError> {
        let Some(to) = target.get("to").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`module_update` step names no `to` package line"));
        };
        let package = self.child_package(to)?;
        serde_json::to_string(&package).map_err(|err| AdapterError::Host(err.to_string()))
    }

    /// §13.10/§13.16 `module_lifecycle_call`: admit a HOST/ROOT-SCOPE transition
    /// that carries module instances through their lifecycle, so a root mutation
    /// spelling `.modules[@id] <- unpack(@package)` (or a `module.<op>` call, or a
    /// §13.16 operator) is lent the privileged handle §13.10 gives the host scope
    /// alone.
    ///
    /// The step names the root `mutation`, its `args`, and — when the program needs
    /// a package — the `packages` label to serialize into a `.liasse` blob and the
    /// parameter (`as`) that blob descriptor binds to. Building the artifact HERE
    /// rather than in a separate step keeps the bytes and the descriptor one fact:
    /// the mutation's `unpack(@package)` reads exactly what was stored.
    ///
    /// Deliberately NOT reachable through `call`: §13.10 lends the lifecycle
    /// privilege by SCOPE, and an external `$public` caller is lent nothing (the
    /// `red/lifecycle-operator-needs-host-privilege` case is that refusal).
    pub(super) fn lifecycle_call(&mut self, target: &serde_json::Value) -> Result<Observation, AdapterError> {
        let Some(mutation) = target.get("mutation").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`module_lifecycle_call` step names no root `mutation`"));
        };
        let args = target.get("args").cloned().unwrap_or(serde_json::Value::Null);
        // §8.3: the root mutation's declared parameter types decode each argument, so
        // a `text` argument arrives as `text` rather than shape-inferred.
        let types = self.mutation_params.get(mutation).cloned().unwrap_or_default();
        let Ok(decoded) = wire::decode_args(&args, &types) else {
            return Ok(Observation::outcome(Outcome::Rejected));
        };
        let mut request = CallRequest::new(mutation.to_owned());
        for (name, value) in decoded {
            request = request.arg(name, value);
        }
        if let Some(label) = target.get("package").and_then(serde_json::Value::as_str) {
            let parameter = target.get("as").and_then(serde_json::Value::as_str).unwrap_or("package");
            let descriptor = self.package_blob(label)?;
            request = request.arg(parameter.to_owned(), Value::Blob(Box::new(descriptor)));
        }
        // §8.2: a ROW mutation's receiver key, in `$key` order. §13.16's bare
        // `.modules[@id]` is written relative to a receiver, so a case exercising
        // that spelling names the row the mutation runs on.
        for component in target.get("receiver").and_then(serde_json::Value::as_array).into_iter().flatten() {
            request = request.receiver(wire::decode_value(component, None));
        }
        match self.deployment.lifecycle_call(&request) {
            Ok(outcome) => Ok(observe_call_outcome(&outcome)),
            // §13.2/§13.3: a mount resolving to no live containing row, or a slot
            // whose name is already taken, refuses the whole transition before
            // anything commits — an admission `rejected`, not a store fault.
            Err(ModuleError::MissingContainingRow(_) | ModuleError::DuplicateName(_) | ModuleError::Unknown(_)) => {
                Ok(Observation::outcome(Outcome::Rejected))
            }
            Err(fault) => Err(AdapterError::Host(format!("module lifecycle call fault: {fault}"))),
        }
    }

    /// Serialize the `packages` entry labelled `label` into a `.liasse` package blob
    /// and store it in the ROOT's §18.3 blob storage, returning the descriptor a
    /// `blob` argument carries. The definition is the package's own JSON, exactly as
    /// `module_install` passes it — one package source, two lifecycle spellings.
    fn package_blob(&mut self, label: &str) -> Result<BlobDescriptor, AdapterError> {
        let package = self.packages.get(label).cloned().ok_or_else(|| {
            AdapterError::unsupported(format!("no package labelled `{label}` in the case's packages map"))
        })?;
        let definition = serde_json::to_vec(&package).map_err(|err| AdapterError::Host(err.to_string()))?;
        let bytes = ArtifactBuilder::new(
            InstanceId::new(format!("{label}#package")),
            HistoryPoint::new(LineageId::new("genesis"), PointId::new("genesis")),
            definition,
            Vec::new(),
            Vec::new(),
        )
        .build()
        .map_err(|err| AdapterError::Host(format!("package artifact build failed: {err}")))?;
        self.deployment
            .store_package_blob(&bytes, Some(format!("{label}.liasse")))
            .map_err(|err| AdapterError::Host(format!("package blob store failed: {err}")))
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
    /// (§13.10): resolve the module collection and instance from the call `args`, forward
    /// the child mutation's own arguments, and admit it against the enabled child.
    pub(super) fn interface_call(
        &mut self,
        iface: &InterfaceRef,
        args: &serde_json::Value,
    ) -> Result<Observation, AdapterError> {
        let Some(resolved) = iface.resolve(args) else {
            return Err(AdapterError::unsupported(
                "an interface-addressed call could not resolve its module collection/instance from \
                 the call arguments",
            ));
        };
        let at = self.entry(&resolved.steps, &resolved.collection, &resolved.instance)?;
        // §13.10: the child mutation receives every argument the selector did not
        // consume (the collection/instance `@param`s address the instance, not the
        // child).
        let forwarded = forward_args(args, &resolved.consumed);
        // §12.1 step 3 / Annex A.1: a forwarded child-mutation argument that does
        // not decode against its declared type is a malformed request, rejected
        // rather than coerced. §13.10: each forwarded argument is typed from the
        // interface contract's declared parameter type, so a cross-module dispatch's
        // `decimal` argument fed a JSON string decodes as a `decimal` (not a `text`)
        // and the owner mutation's typed metered assert sees the value it declared.
        // A parameter the contract leaves untyped is shape-inferred (§8.3).
        let Ok(forwarded_args) = wire::decode_args(&forwarded, &iface.param_types) else {
            return Ok(Observation::outcome(Outcome::Rejected));
        };
        let mut request = CallRequest::new(String::new());
        for (name, value) in forwarded_args {
            request = request.arg(name, value);
        }
        match self.deployment.interface_call(&at, &resolved.interface, &resolved.mutation, &request) {
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

    /// The module collection an install step's `at` display path names: the path's
    /// last component is the collection, the pairs before it address its containing
    /// row. Resolved against LIVE root state, so an address is built from the rows'
    /// own typed keys rather than parsed out of the text.
    fn collection(&self, target: &serde_json::Value) -> Result<CollectionPath, AdapterError> {
        let Some(path) = target.get("at").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`module_install` step names no `at` collection path"));
        };
        let (steps, name) = display_path(path)
            .ok_or_else(|| AdapterError::Host(format!("malformed module collection path `{path}`")))?;
        // A containing row that is NOT live still yields an address: whether the
        // module collection exists there is the HOST's §13.2 judgement, and it
        // answers with a `MissingContainingRow` rejection. Resolving it away here
        // would turn that rejection into a driver error.
        Ok(self
            .deployment
            .root()
            .resolve_collection_path(&steps, &name)
            .map_err(|error| AdapterError::Host(format!("module collection path `{path}`: {error}")))?
            .unwrap_or_else(|| textual_collection(&steps, &name)))
    }

    /// The module-collection entry a lifecycle step's `instance` display path names
    /// (§13.3): the trailing component is the instance name, the prefix addresses
    /// the collection.
    fn instance(&self, target: &serde_json::Value) -> Result<RowAddress, AdapterError> {
        let Some(path) = target.get("instance").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("module lifecycle step names no `instance` path"));
        };
        let Some((collection, name)) = path.rsplit_once('/').filter(|(c, n)| !c.is_empty() && !n.is_empty()) else {
            return Err(AdapterError::Host(format!("malformed instance path `{path}`")));
        };
        let (steps, member) = display_path(collection)
            .ok_or_else(|| AdapterError::Host(format!("malformed instance path `{path}`")))?;
        self.entry(&steps, &member, name)
    }

    /// The address of entry `name` in the module collection `member` under the row
    /// `steps` addresses. §13.3 makes an instance name a text value, so the entry
    /// key is that text — no key parsing is involved.
    fn entry(
        &self,
        steps: &[(String, String)],
        member: &str,
        name: &str,
    ) -> Result<RowAddress, AdapterError> {
        let collection = self
            .deployment
            .root()
            .resolve_collection_path(steps, member)
            .map_err(|error| AdapterError::Host(format!("module collection `{member}`: {error}")))?
            .unwrap_or_else(|| textual_collection(steps, member));
        Ok(collection.row(KeyValue::single(Value::Text(Text::new(name)))))
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
/// name, unknown/disabled instance, or absent containing row is an admission
/// `rejected` — bad input, never an error.
fn observe(observation: ModuleObservation) -> Observation {
    match observation {
        ModuleObservation::Applied => Observation::ok(None),
        ModuleObservation::EmptyName | ModuleObservation::InvalidBinding(_) => {
            Observation::outcome(Outcome::Invalid)
        }
        ModuleObservation::DuplicateName(_)
        | ModuleObservation::Unknown(_)
        | ModuleObservation::Disabled(_)
        | ModuleObservation::MissingContainingRow(_)
        // §13.5: an unresolvable required peer binding is an admission-time refusal.
        | ModuleObservation::PeerUnresolved(_) => Observation::outcome(Outcome::Rejected),
    }
}

/// Assemble the §13.15 update-report value from the runtime [`ModuleUpdateReport`]
/// and the instance display path the driver knows: `$instance`, `$from`, `$to`,
/// `$migrated`, `$seeded`, the `$exposed` grouping, the `$imports` grouping, and
/// `$commit`. Per §13.15 `$migrated`/`$seeded` are per-item lists in canonical path
/// order and `$exposed`/`$imports` group names in canonical text order.
fn update_report_value(instance: &str, report: &ModuleUpdateReport) -> serde_json::Value {
    serde_json::json!({
        "$instance": instance,
        "$from": report.from,
        "$to": report.to,
        "$migrated": report.migrated,
        "$seeded": report.seeded,
        "$exposed": {
            "$unchanged": report.exposed_unchanged,
            "$changed": report.exposed_changed,
            "$removed": report.exposed_removed,
        },
        "$imports": {
            "$rebound": report.imports_rebound,
            "$broken": report.imports_broken,
        },
        "$commit": report.commit.get(),
    })
}

/// Read a flat `[collection, key, collection, key, …]` walk as its `(collection,
/// key)` steps. `None` for an odd count — that names no single row, so it is
/// refused rather than silently truncated.
fn pairs(components: &[impl AsRef<str>]) -> Option<Vec<(String, String)>> {
    let mut steps = Vec::with_capacity(components.len() / 2);
    let mut walk = components.iter();
    loop {
        let Some(collection) = walk.next() else { return Some(steps) };
        let key = walk.next()?;
        steps.push((collection.as_ref().to_owned(), key.as_ref().to_owned()));
    }
}

/// The module collection `steps`/`name` address when no live containing row backs
/// it: every step key read as the `text` value it is written as. §13.2 makes the
/// existence of that containing row the host's judgement, so this builds the
/// address the host then rejects by name rather than deciding here.
fn textual_collection(steps: &[(String, String)], name: &str) -> CollectionPath {
    use liasse_ident::NameSegment;
    use liasse_store::AddressStep;
    let segment = NameSegment::new(name);
    if steps.is_empty() {
        return CollectionPath::top(segment);
    }
    CollectionPath::nested(
        steps.iter().map(|(collection, key)| {
            AddressStep::new(NameSegment::new(collection), KeyValue::single(Value::Text(Text::new(key.clone()))))
        }),
        segment,
    )
}

/// Split a collection display path (`/companies/acme/modules`) into the
/// `(collection, key)` steps of its containing row and the collection's own
/// declaration name. `None` when the path is not an absolute, alternating
/// collection/key walk ending in a collection name.
fn display_path(path: &str) -> Option<(Vec<(String, String)>, String)> {
    let body = path.strip_prefix('/')?;
    let mut components: Vec<&str> = body.split('/').collect();
    if components.iter().any(|c| c.is_empty()) {
        return None;
    }
    let name = components.pop()?.to_owned();
    Some((pairs(&components)?, name))
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

/// The call `args` restricted to the members the collection/instance selectors did not
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
    // The module collection's `$interfaces` contracts declare each routed mutation's
    // parameter types (§13.8); a forwarded dispatch argument decodes against them.
    let contracts = interface_contracts(package);
    if let Some(public) = model.get("$public").and_then(serde_json::Value::as_object) {
        collect_surface_interface_calls("public", public, &contracts, &mut map);
    }
    if let Some(roles) = model.get("$roles").and_then(serde_json::Value::as_object) {
        for (role, definition) in roles {
            if let Some(surfaces) = definition.as_object() {
                collect_surface_interface_calls(role, surfaces, &contracts, &mut map);
            }
        }
    }
    map
}

/// Record each surface's interface-addressed `$mut` calls under `prefix`, typing each
/// routed mutation from its `$interfaces` contract in `contracts`.
fn collect_surface_interface_calls(
    prefix: &str,
    surfaces: &serde_json::Map<String, serde_json::Value>,
    contracts: &BTreeMap<(String, String), BTreeMap<String, Type>>,
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
            if let Some(mut iface) = body.as_str().and_then(InterfaceRef::parse) {
                if let Some(types) = contracts.get(&(iface.interface.clone(), iface.mutation.clone())) {
                    iface.param_types = types.clone();
                }
                map.insert(format!("{prefix}.{surface}.{call}"), iface);
            }
        }
    }
}

/// The root package's declared `$model.$mut` parameter types, keyed by mutation
/// name (§8.3). A root `$mut` key carries the same `name({ a: text, b: blob })`
/// signature an interface contract does, so it parses through the same reader — a
/// `module_lifecycle_call` names a root mutation directly, so the router's
/// surface-keyed argument types do not answer for it.
fn root_mutation_params(package: &serde_json::Value) -> BTreeMap<String, BTreeMap<String, Type>> {
    let mut out = BTreeMap::new();
    let Some(muts) =
        package.get("$model").and_then(|model| model.get("$mut")).and_then(serde_json::Value::as_object)
    else {
        return out;
    };
    for key in muts.keys() {
        let (mutation, types) = parse_interface_signature(key);
        out.insert(mutation, types);
    }
    out
}

/// The declared parameter types of every module-collection interface mutation in a
/// root package, keyed by `(interface, mutation)` (§13.8/§13.10). A module
/// collection's `$interfaces.<name>.$mut` keys carry the mutation's typed signature
/// (`consume({ amount: decimal })`), so a cross-module dispatch's forwarded arguments
/// can be typed against the contract rather than shape-inferred.
fn interface_contracts(package: &serde_json::Value) -> BTreeMap<(String, String), BTreeMap<String, Type>> {
    let mut out = BTreeMap::new();
    if let Some(model) = package.get("$model") {
        collect_interface_contracts(model, &mut out);
    }
    out
}

/// Recursively harvest every module collection's `$interfaces` contract's declared
/// mutation parameter types from a model subtree (a module collection can be nested
/// under any row, §13.2).
fn collect_interface_contracts(
    node: &serde_json::Value,
    out: &mut BTreeMap<(String, String), BTreeMap<String, Type>>,
) {
    let Some(object) = node.as_object() else {
        return;
    };
    if let Some(interfaces) = object.get("$interfaces").and_then(serde_json::Value::as_object) {
        for (interface, definition) in interfaces {
            let Some(muts) = definition.get("$mut").and_then(serde_json::Value::as_object) else {
                continue;
            };
            for key in muts.keys() {
                let (mutation, types) = parse_interface_signature(key);
                if !types.is_empty() {
                    out.insert((interface.clone(), mutation), types);
                }
            }
        }
    }
    for value in object.values() {
        collect_interface_contracts(value, out);
    }
}

/// Parse an interface `$mut` key into its mutation name and declared parameter types.
/// A signature `consume({ amount: decimal })` yields `("consume", { amount: decimal })`;
/// a bare name (`consume`) declares no typed parameters.
fn parse_interface_signature(key: &str) -> (String, BTreeMap<String, Type>) {
    let key = key.trim();
    let Some(open) = key.find('(') else {
        return (key.to_owned(), BTreeMap::new());
    };
    let name = key[..open].trim().to_owned();
    let inner = key[open + 1..].trim_end().strip_suffix(')').unwrap_or_default().trim();
    let types = match parse_type(inner) {
        Type::Struct(fields) => {
            fields.fields().map(|(name, ty)| (name.clone(), ty.clone())).collect()
        }
        _ => BTreeMap::new(),
    };
    (name, types)
}

/// A component of an interface-addressed reference's collection/instance path:
/// either a literal key or a `@param` resolved from the call arguments.
#[derive(Debug, Clone)]
enum PathSeg {
    Lit(String),
    Param(String),
}

/// A parsed interface-addressed surface `$mut` reference (§13.10), e.g.
/// `/companies[@company].modules[@module]::templates.create`: the module-collection
/// path template, the instance-name selector, the interface, and the routed
/// mutation.
#[derive(Debug, Clone)]
pub(super) struct InterfaceRef {
    collection: Vec<PathSeg>,
    instance: PathSeg,
    interface: String,
    mutation: String,
    /// The declared parameter types of the routed mutation, taken from the module
    /// collection's `$interfaces` contract (§13.8/§13.10). A forwarded cross-module
    /// dispatch argument decodes against its declared type here — so a `decimal`
    /// parameter fed a JSON string (`"4"`) becomes a `decimal`, not a `text`, and
    /// the owner mutation's typed metered assert sees the value it declared. Empty
    /// when the contract declares no typed signature for the mutation.
    param_types: BTreeMap<String, Type>,
}

/// An [`InterfaceRef`] resolved against a call's arguments.
struct ResolvedInterfaceCall {
    /// The `(collection, key)` steps addressing the module collection's containing
    /// row.
    steps: Vec<(String, String)>,
    /// The module collection's own declaration name.
    collection: String,
    instance: String,
    interface: String,
    mutation: String,
    /// The argument names the collection/instance selectors consumed.
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
            collection: walk_space(space_expr)?,
            instance: key_seg(instance_key)?,
            interface: interface.text.clone(),
            mutation: mutation.text.clone(),
            // Filled from the package's `$interfaces` contract by the binding
            // collector, which has the whole package in view.
            param_types: BTreeMap::new(),
        })
    }

    /// Resolve the module collection and instance name against the call `args`.
    fn resolve(&self, args: &serde_json::Value) -> Option<ResolvedInterfaceCall> {
        let mut consumed = BTreeSet::new();
        let mut components = Vec::new();
        for seg in &self.collection {
            components.push(resolve_seg(seg, args, &mut consumed)?);
        }
        let collection = components.pop()?;
        let steps = pairs(&components)?;
        let instance = resolve_seg(&self.instance, args, &mut consumed)?;
        Some(ResolvedInterfaceCall {
            steps,
            collection,
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
