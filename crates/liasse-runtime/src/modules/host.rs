//! The module composition host: a root engine plus the child instances mounted in
//! its row-scoped module spaces (§13).

use std::collections::BTreeMap;

use liasse_expr::{Cell, ExprType};
use liasse_ident::InstanceId;
use liasse_store::StoreFactory;
use liasse_value::{Type, Value};

use crate::engine::Engine;
use crate::error::EngineError;
use crate::generator::Generators;
use crate::imports::ParentImports;
use crate::modules::install::{AdmittedBindings, InstallRequest, UseSpec};
use crate::modules::peer::{self, ResolvedPeer, SiblingInterface};
use crate::modules::{AggregatedInstance, InterfaceRow, ModuleAggregate, ModuleError, ModuleSpace};
use crate::outcome::CallOutcome;
use crate::request::{CallRequest, ViewQuery};
use crate::view::ViewResult;

/// One installed child module instance mounted in a space.
struct Child<S> {
    /// The module space this instance is mounted in (§13.2).
    space: ModuleSpace,
    /// The instance name, the local component of its identity within the space
    /// (§13.3). Mutable: a rename is a rekey (§13.3).
    name: String,
    /// The immutable incarnation, preserved across rename (D.1).
    incarnation: InstanceId,
    /// The child's own loaded engine over its private store — a wholly separate
    /// instance, so isolation is structural (§13.1).
    engine: Engine<S>,
    /// The boundary bindings admitted at install (§13.3 `$resolved`).
    bindings: AdmittedBindings,
    /// The §13.5 peer handles resolved against the sibling set at install (§13.3
    /// `$resolved`/`$absent`): each records the concrete sibling instance a handle
    /// bound to, or its absence for an optional peer. Consulted at every interface
    /// read so `#handle` evaluates against that sibling's current interface state.
    resolved_peers: Vec<ResolvedPeer>,
    /// Whether the child's active boundary occurrences are available (§13.3/§13.12
    /// disable/enable). A disabled child keeps its private state and history.
    enabled: bool,
}

impl<S> Child<S> {
    fn is(&self, space: &ModuleSpace, name: &str) -> bool {
        &self.space == space && self.name == name
    }
}

/// A root application together with the module instances installed in its
/// row-scoped module spaces (§13.2). Each child is an independently loaded
/// [`Engine`] over a store the host's [`StoreFactory`] mints, so two installs of
/// the same package — in the same space under different names, or in two spaces —
/// are isolated instances (§13.1/§13.2).
pub struct ModuleHost<F: StoreFactory> {
    factory: F,
    root: Engine<F::Store>,
    children: Vec<Child<F::Store>>,
    next_incarnation: u64,
}

impl<F: StoreFactory> ModuleHost<F> {
    /// Wrap a loaded root engine, ready to install module instances.
    pub fn new(factory: F, root: Engine<F::Store>) -> Self {
        Self { factory, root, children: Vec::new(), next_incarnation: 0 }
    }

    /// The root application engine.
    #[must_use]
    pub fn root(&self) -> &Engine<F::Store> {
        &self.root
    }

    /// The root application engine, mutably (to admit root requests).
    pub fn root_mut(&mut self) -> &mut Engine<F::Store> {
        &mut self.root
    }

    /// Install a new instance into `space` from an install `request` (§13.3),
    /// admitting its `$config`/`$use`/`$deps` boundary bindings. Rejects an empty or
    /// duplicate name and a malformed binding; otherwise mints a fresh incarnation,
    /// creates the child's private store, loads its engine (applying its own `$data`
    /// seed), and records the admitted bindings on the instance.
    ///
    /// The `$config` values are type-checked and bound (§13.1); peer resolution
    /// against the sibling set (§13.5) remains a documented seam, its bindings
    /// recorded so a later pass can apply them.
    pub fn install<G: Generators>(
        &mut self,
        space: &ModuleSpace,
        request: InstallRequest,
        generator: &mut G,
    ) -> Result<InstanceId, ModuleError> {
        let admitted = request.admit()?;
        let name = admitted.name;
        // §13.2/§13.3: an install creates an instance "inside an existing module
        // space", and a `$modules` space exists only at the location of its
        // containing row. Reject an install whose containing row is not live in root
        // state (a ghost-row space like `/companies/ghost/modules`) before minting an
        // incarnation or a store — nothing may be installed into a space that does
        // not exist.
        self.check_containing_row(space)?;
        if self.find(space, &name).is_some() {
            return Err(ModuleError::DuplicateName(name));
        }
        // §13.5: resolve every peer `$use` handle against the enabled sibling set in
        // this space BEFORE loading the child — one candidate auto-binds, several
        // require an explicit `$use` binding, zero rejects a required binding — so an
        // unresolvable required peer refuses the install before an incarnation or
        // store is minted. An optional peer with no candidate resolves as absent.
        let resolved_peers = peer::resolve(space, &admitted.bindings, &self.siblings(space))?;
        // §13.4/§13.5: resolve the parent surfaces this child imports (`company:
        // "$parent"`) row-local against the space's containing row, and bind each
        // resolved peer handle to its sibling's exposed interface, BEFORE loading the
        // child — so its compile types `#company`/`#people` and its genesis seed
        // evaluates against the projected parent row and the bound peer view.
        let imports = self.child_imports(space, &admitted.bindings, &resolved_peers)?;
        let incarnation = self.mint_incarnation(space, &name);
        let store = self
            .factory
            .create(incarnation.clone())
            .map_err(|error| ModuleError::Engine(EngineError::Store(error)))?;
        // §13.1/§13.3/§9.1: type-check the supplied `$config` values against the
        // child's declared `$config` struct (rejecting an unknown member or a type
        // mismatch), resolve omitted members from their defaults, and bind the
        // resolved struct as the `$config` value the child's expressions read — BEFORE
        // the package genesis `$seed`/`$data` seed runs, so a genesis field default may
        // read `$config` (a seed row passes through the same default rules a mutation
        // insert does on the installed, config-bound instance). The subsequent
        // installation `$data` overlay likewise reads the bound `$config`.
        let mut engine =
            Engine::install_load(store, &admitted.definition, &admitted.bindings.config, &imports, generator)
                .map_err(|error| match error {
                    crate::config::ConfigBindError::Mismatch(mismatch) => ModuleError::ConfigMismatch(mismatch.to_string()),
                    crate::config::ConfigBindError::Engine(engine) => ModuleError::Engine(engine),
                })?;
        // §13.8/§13.3: the child's `$expose` must structurally satisfy the module
        // space's declared `$interfaces` contract before the instance activates. The
        // contract check reads only the compiled exposed views, so it is unaffected by
        // whether genesis has run.
        self.check_interface_contracts(space, &engine)?;
        // §13.3: package `$data` was applied by the load; the installation `$data`
        // now overlays onto the child genesis, passing ordinary insertion validation.
        if let Some(data) = &admitted.data {
            engine.overlay_install_data(data, &imports, generator)?;
        }
        let bindings = admitted.bindings;
        self.children.push(Child {
            space: space.clone(),
            name,
            incarnation: incarnation.clone(),
            engine,
            bindings,
            resolved_peers,
            enabled: true,
        });
        Ok(incarnation)
    }

    /// Whether an instance of that name is installed in `space` (enabled or
    /// disabled).
    #[must_use]
    pub fn is_installed(&self, space: &ModuleSpace, name: &str) -> bool {
        self.find(space, name).is_some()
    }

    /// Whether the named instance in `space` is installed and enabled.
    #[must_use]
    pub fn is_enabled(&self, space: &ModuleSpace, name: &str) -> bool {
        self.find(space, name).is_some_and(|child| child.enabled)
    }

    /// The incarnation of the named instance in `space`, if installed.
    #[must_use]
    pub fn incarnation(&self, space: &ModuleSpace, name: &str) -> Option<&InstanceId> {
        self.find(space, name).map(|child| &child.incarnation)
    }

    /// The admitted boundary bindings of the named instance in `space` (§13.3).
    #[must_use]
    pub fn bindings(&self, space: &ModuleSpace, name: &str) -> Option<&AdmittedBindings> {
        self.find(space, name).map(|child| &child.bindings)
    }

    /// Disable an instance (§13.3, §13.12): remove its active boundary occurrences,
    /// external surfaces, and peer availability while retaining its private stored
    /// state and history. The child engine and store are kept intact, so a later
    /// [`ModuleHost::enable`] restores the exact preserved state.
    pub fn disable(&mut self, space: &ModuleSpace, name: &str) -> Result<(), ModuleError> {
        self.child_mut(space, name)?.enabled = false;
        Ok(())
    }

    /// Enable a disabled instance (§13.3): revalidate and restore its boundary
    /// occurrences over the exact preserved private state.
    pub fn enable(&mut self, space: &ModuleSpace, name: &str) -> Result<(), ModuleError> {
        self.child_mut(space, name)?.enabled = true;
        Ok(())
    }

    /// Uninstall an instance (§13.3, §13.12): remove the instance incarnation and
    /// its owned subtree.
    pub fn uninstall(&mut self, space: &ModuleSpace, name: &str) -> Result<(), ModuleError> {
        match self.children.iter().position(|child| child.is(space, name)) {
            Some(index) => {
                self.children.remove(index);
                Ok(())
            }
            None => Err(ModuleError::Unknown(name.to_owned())),
        }
    }

    /// Rename an instance within its space (§13.3): a rekey that preserves the
    /// incarnation and therefore the durable identity (D.1). Rejects a name already
    /// in use in the same space.
    pub fn rename(&mut self, space: &ModuleSpace, from: &str, to: &str) -> Result<(), ModuleError> {
        if self.find(space, to).is_some() {
            return Err(ModuleError::DuplicateName(to.to_owned()));
        }
        self.child_mut(space, from)?.name = to.to_owned();
        Ok(())
    }

    /// Update a single instance to a target definition (§13.14/§13.15), affecting
    /// that instance only.
    ///
    /// Before the migration commits, the target's exposed compatibility surface is
    /// rechecked against the active one (§13.14): a minor/patch update that
    /// definitionally narrows the module's own `$expose` is refused
    /// ([`ModuleError::ExposedNarrowed`]), and one that withdraws a previously
    /// accepted interface binding whose implementation remains is refused
    /// ([`ModuleError::InterfaceBindingWithdrawn`]) — in both the current release
    /// stays active (E.9). A preserving/widening update runs the §20 migration over
    /// the child's own engine and returns the assembled §13.15 report.
    pub fn update<G: Generators>(
        &mut self,
        space: &ModuleSpace,
        name: &str,
        target: &str,
        generator: &mut G,
    ) -> Result<crate::modules::ModuleUpdateReport, ModuleError> {
        // §13.14: read the ACTIVE child definition and version for the exposed-surface
        // recheck and the §13.15 `$from`, before any migration mutates the child.
        let child = self.find(space, name).ok_or_else(|| ModuleError::Unknown(name.to_owned()))?;
        let active_definition = child.engine.definition_source()?.ok_or_else(|| {
            ModuleError::Engine(EngineError::Internal("active child definition unavailable for update".to_owned()))
        })?;
        let from = version_string(child.engine.model());
        // §13.14: a minor/patch update MUST preserve or widen the module's exposed
        // compatibility surface. Refuse a narrowing release before admission (E.9),
        // classifying a definitional self-narrowing as a static refusal and a
        // withdrawn-but-implemented binding as an admission refusal.
        if let Some(narrowing) = super::compat::exposed_narrowing(&active_definition, target) {
            return Err(match narrowing.class {
                super::compat::NarrowingClass::Definitional => ModuleError::ExposedNarrowed(narrowing.reason),
                super::compat::NarrowingClass::BindingWithdrawn => {
                    ModuleError::InterfaceBindingWithdrawn(narrowing.reason)
                }
            });
        }
        // §13.15 `$exposed`: group each exposed interface by how its contract moved
        // across this (non-narrowing) update.
        let grouping = super::compat::exposed_grouping(&active_definition, target);
        // §13.14: run the §20 migration over the child's own engine.
        let child = self.child_mut(space, name)?;
        let report = child.engine.update(target, generator).map_err(|error| match error {
            crate::migrate::UpdateError::Engine(engine) => ModuleError::Engine(engine),
            other => ModuleError::Engine(EngineError::Internal(other.to_string())),
        })?;
        let to = version_string(child.engine.model());
        Ok(crate::modules::ModuleUpdateReport {
            from,
            to,
            commit: report.commit,
            migrated: report.migrated,
            seeded: report.seeded,
            exposed_unchanged: grouping.unchanged,
            exposed_changed: grouping.changed,
            exposed_removed: grouping.removed,
            // §13.15 `$imports`: an import re-bound by the update is `$rebound`, one
            // whose source is gone is `$broken`. The CORE module cases carry no
            // `$use` imports, so both are empty; recomputing per bound parent/peer
            // source under the migrated model is a follow-on.
            imports_rebound: Vec::new(),
            imports_broken: Vec::new(),
        })
    }

    /// Admit a mutation call against an enabled child instance (§13.11 direct
    /// module surface). A disabled instance has no active surfaces.
    pub fn child_call<G: Generators>(
        &mut self,
        space: &ModuleSpace,
        name: &str,
        request: &CallRequest,
        generator: &mut G,
    ) -> Result<CallOutcome, ModuleError> {
        let child = self.enabled_child_mut(space, name)?;
        child.engine.call(request, generator).map_err(ModuleError::Engine)
    }

    /// Read an enabled child instance's exposed interface `$view` through the
    /// boundary (§13.8): only the fields the exposed projection selects cross, so a
    /// private field is unreachable here (§13.8 isolation). A disabled instance
    /// exposes no boundary occurrences (§13.12). `None` when the child declares no
    /// readable interface of that name.
    pub fn interface_read(
        &self,
        space: &ModuleSpace,
        name: &str,
        interface: &str,
    ) -> Result<Option<ViewResult>, ModuleError> {
        let child = self.enabled_child(space, name)?;
        let imports = self.child_imports(space, &child.bindings, &child.resolved_peers)?;
        child.engine.interface_read(interface, &imports).map_err(ModuleError::Engine)
    }

    /// Aggregate one exposed interface across every enabled instance in `space`
    /// (§13.9 "The parent reads every instance exposing an interface"). Each row
    /// carries its inherited identity — the instance name plus the exposed row
    /// (§13.9). A disabled instance is skipped, so disabling removes it from the
    /// aggregation (§13.12); instances are visited in installation order.
    pub fn aggregate(
        &self,
        space: &ModuleSpace,
        interface: &str,
    ) -> Result<Vec<InterfaceRow>, ModuleError> {
        let mut rows = Vec::new();
        for child in self.children.iter().filter(|c| &c.space == space && c.enabled) {
            let imports = self.child_imports(&child.space, &child.bindings, &child.resolved_peers)?;
            let Some(result) = child.engine.interface_read(interface, &imports).map_err(ModuleError::Engine)? else {
                continue;
            };
            for row in result.rows() {
                rows.push(InterfaceRow { instance: child.name.clone(), row: row.clone() });
            }
        }
        Ok(rows)
    }

    /// Evaluate a named child view at head — the §13.11 *direct* module surface a
    /// host mounts, distinct from the [`ModuleHost::interface_read`] boundary read.
    /// Only an enabled instance exposes its surfaces (§13.12).
    pub fn child_view(&self, space: &ModuleSpace, name: &str, view: &str) -> Result<Option<ViewResult>, ModuleError> {
        let child = self.enabled_child(space, name)?;
        child.engine.view_at_head(view).map_err(ModuleError::Engine)
    }

    /// Evaluate a **root** package view that reads its installed children through
    /// `.modules::iface` (§13.9), with the enabled child instances folded into the
    /// read. This is what makes module composition visible to the root engine: the
    /// host aggregates each enabled child's exposed interface `$view` through the
    /// boundary (§13.8 — only projected fields cross, so a private field stays
    /// unreachable) and evaluates the named root view over the resulting module
    /// spaces, so a `catalog: ".modules::iface { module: modules.$key, … }"`
    /// aggregation resolves against the actual children. Serves a plain `$view`, a
    /// `$public`/role surface `$view` (bind `$params`/`$actor` via `query`), and a
    /// nested `/collection[k].catalog` view. `None` when no view of that name is
    /// declared.
    pub fn root_view(&self, name: &str, query: &ViewQuery) -> Result<Option<ViewResult>, ModuleError> {
        let aggregate = self.aggregate_snapshot()?;
        let frontier = self.root.head().map_err(ModuleError::Engine)?;
        self.root.view_with_modules(name, frontier, query, &aggregate).map_err(ModuleError::Engine)
    }

    /// Dispatch an interface-addressed mutation to a child's `$expose`d mutation
    /// (§13.10): resolve `interface.mutation` on the enabled instance in `space` to
    /// the private mutation it binds and admit it against the child atomically,
    /// returning the child mutation's response (the §13.8 `$return` shape). This is
    /// the "a parent routes a call to a child's exposed mutation" boundary; the
    /// binding must be a simple root-mutation reference (`.create_template`) — a
    /// row-scoped or inline binding, and folding the child transition into the same
    /// atomic *parent* transition (§13.10/§13.11), remain documented seams.
    ///
    /// # Errors
    /// [`ModuleError::Unknown`]/[`ModuleError::Disabled`] for an absent or disabled
    /// instance; [`ModuleError::InterfaceContract`] when the interface binds no such
    /// routable mutation; an engine/store fault otherwise. A rejected child
    /// transition is a [`CallOutcome::Rejected`], not an error.
    pub fn interface_call<G: Generators>(
        &mut self,
        space: &ModuleSpace,
        name: &str,
        interface: &str,
        mutation: &str,
        request: &CallRequest,
        generator: &mut G,
    ) -> Result<CallOutcome, ModuleError> {
        let child = self.enabled_child(space, name)?;
        // §13.8: an exposed mutation binding a private child mutation (`.create`)
        // routes to the child engine.
        if let Some(child_mutation) = child.engine.exposed_mutation(interface, mutation) {
            let routed = request.clone().with_mutation(child_mutation);
            let child = self.enabled_child_mut(space, name)?;
            return child.engine.call(&routed, generator).map_err(ModuleError::Engine);
        }
        // §13.4: an exposed mutation binding a parent surface (`#company.rename(…)`)
        // delegates to the parent capability, whose effect lands on the parent row
        // the space is scoped to — admitted against the root engine.
        if let Some(routed) = self.parent_mutation_request(space, name, interface, mutation, request)? {
            return self.root.call(&routed, generator).map_err(ModuleError::Engine);
        }
        Err(ModuleError::InterfaceContract(
            interface.to_owned(),
            format!("interface binds no routable mutation `{mutation}`"),
        ))
    }

    /// Build the root [`CallRequest`] a §13.4 parent-surface-delegating exposed
    /// mutation routes to (`#company.rename({ name: @name })`): resolve the imported
    /// handle to its parent surface, map the parent mutation contract to the
    /// containing-row mutation it binds (`.rename`), address the space's containing
    /// row as the receiver, and feed each parent parameter from the child call's
    /// arguments. `None` when the exposed mutation is not a parent-surface
    /// delegation, its handle resolves to no parent surface, or its argument form is
    /// outside the CORE route.
    fn parent_mutation_request(
        &self,
        space: &ModuleSpace,
        name: &str,
        interface: &str,
        mutation: &str,
        request: &CallRequest,
    ) -> Result<Option<CallRequest>, ModuleError> {
        use crate::modules::parent::{ArgSource, ParentMutationBinding};
        let child = self.enabled_child(space, name)?;
        let Some(binding) = child.engine.exposed_mutation_binding(interface, mutation) else {
            return Ok(None);
        };
        let Some(parsed) = ParentMutationBinding::parse(binding) else {
            return Ok(None);
        };
        // Resolve the imported handle (`company`) to the parent surface it names.
        let surface = child.bindings.uses.iter().find_map(|(handle, spec, _)| {
            if handle != &parsed.handle {
                return None;
            }
            match spec {
                UseSpec::Parent => Some(handle.as_str()),
                UseSpec::ParentSurface(surface) => Some(surface.as_str()),
                UseSpec::Path(_) | UseSpec::Peer { .. } => None,
            }
        });
        let Some(surface) = surface else {
            return Ok(None);
        };
        let declaration = space.declaration_path();
        let steps = space.containing_row_steps().unwrap_or_default();
        let Some(resolved) = self.root.parent_surface_projection(&declaration, &steps, surface)? else {
            return Ok(None);
        };
        // §13.4: the parent surface `$mut` maps the contract (`rename`) to the
        // containing-row mutation it binds (`.rename` → the `rename` mutation).
        let Some(root_mutation) = resolved
            .muts
            .iter()
            .find(|(contract, _)| contract == &parsed.mutation)
            .and_then(|(_, binding)| binding.strip_prefix('.'))
            .map(|m| m.strip_suffix("()").unwrap_or(m).to_owned())
        else {
            return Ok(None);
        };
        // The receiver is the space's containing row (`/companies/globex`).
        let mut routed = CallRequest::new(root_mutation);
        for (_, key) in &steps {
            routed = routed.receiver(liasse_value::Value::Text(liasse_value::Text::new(key.clone())));
        }
        // Feed each parent parameter from the child call's arguments (§13.4).
        for (param, source) in &parsed.args {
            let ArgSource::Param(child_arg) = source;
            if let Some(value) = request.arg_value(child_arg) {
                routed = routed.arg(param.clone(), value.clone());
            }
        }
        Ok(Some(routed))
    }

    /// Aggregate every enabled child's exposed interface rows into the snapshot the
    /// root engine folds into a `.modules::iface` read (§13.9). Each instance is
    /// grouped under its module-space display path, carrying one entry per readable
    /// interface it exposes (its boundary-projected rows). A disabled instance is
    /// skipped, so it leaves the aggregation (§13.12).
    fn aggregate_snapshot(&self) -> Result<ModuleAggregate, ModuleError> {
        let mut spaces: BTreeMap<String, Vec<AggregatedInstance>> = BTreeMap::new();
        for child in self.children.iter().filter(|c| c.enabled) {
            // §13.4/§13.5: re-resolve this child's parent-surface and peer imports
            // live, so an `$expose` `$view` reading `#company`/`#people` reflects the
            // parent's current state and each bound peer's current interface rows.
            let imports = self.child_imports(&child.space, &child.bindings, &child.resolved_peers)?;
            let names: Vec<String> = child.engine.exposed_interface_names().map(str::to_owned).collect();
            let mut interfaces = Vec::new();
            for interface in names {
                if let Some(rows) = child.engine.interface_rows(&interface, &imports).map_err(ModuleError::Engine)? {
                    interfaces.push((interface, rows));
                }
            }
            spaces
                .entry(child.space.as_str().to_owned())
                .or_default()
                .push(AggregatedInstance { name: child.name.clone(), interfaces });
        }
        Ok(ModuleAggregate::new(spaces))
    }

    /// Check the `child` engine's `$expose` satisfies every interface contract the
    /// module space at `space` declares in the root package (§13.8/§13.10): the
    /// exposed `$view` output must carry each declared field with a matching type
    /// (view satisfaction is structural), and every bound `$mut` must satisfy its
    /// declared parameter and response contracts — reading only the parameters the
    /// interface prototype supplies and projecting every `$return` field with the
    /// declared type. A space the root declares no contract for (an undeclared
    /// space, a documented §13.2 seam) imposes none.
    fn check_interface_contracts(&self, space: &ModuleSpace, child: &Engine<F::Store>) -> Result<(), ModuleError> {
        let Some(contracts) = self.root.module_space_interfaces(&space.declaration_path()) else {
            return Ok(());
        };
        for contract in contracts {
            // §13.9: an instance exposes only the interfaces it implements — the
            // parent reads "every instance exposing an interface". A child that does
            // not expose this one simply does not implement it, so there is nothing
            // to check; only an *exposed* view must satisfy the declared contract.
            if let Some(exposed) = child.exposed_view_fields(&contract.name) {
                for (field, ty) in &contract.view_fields {
                    let satisfied = exposed.iter().any(|(name, got)| name == field && got == ty);
                    if !satisfied {
                        return Err(ModuleError::InterfaceContract(
                            contract.name.clone(),
                            format!("the exposed view does not provide field `{field}` with the declared type"),
                        ));
                    }
                }
            }
            self.check_interface_muts(contract, child)?;
        }
        Ok(())
    }

    /// Check each `$mut` contract the interface declares is satisfied by the private
    /// mutation the child binds for it (§13.8/§13.10). For a contract the child
    /// binds to a resolvable root mutation: every parameter the mutation reads MUST
    /// be one the interface prototype supplies (the boundary supplies no others), and
    /// every `$return` field MUST appear in the mutation's response projection with
    /// the declared type. A contract the child does not bind, or binds through a
    /// row-scoped/inline program the check cannot resolve, is a documented seam.
    fn check_interface_muts(
        &self,
        contract: &crate::compiled::CompiledInterfaceContract,
        child: &Engine<F::Store>,
    ) -> Result<(), ModuleError> {
        for imut in &contract.muts {
            let Some(bound) = child.exposed_mutation_contract(&contract.name, &imut.name) else {
                continue;
            };
            // §13.8 parameter contract: the bound mutation may read only parameters
            // the interface prototype declares; a parameter the prototype omits is
            // never supplied across the boundary.
            if let Some(declared) = &imut.params {
                for param in &bound.params {
                    if !declared.iter().any(|name| name == param) {
                        return Err(ModuleError::InterfaceContract(
                            contract.name.clone(),
                            format!(
                                "the bound mutation `{}` reads parameter `{param}`, which the interface \
                                 parameter contract does not declare",
                                imut.name
                            ),
                        ));
                    }
                }
            }
            // §13.8 response contract: every `$return` field must appear in the
            // bound mutation's response projection with the declared type. An opaque
            // response (a non-projection return) is left uncompared (a seam).
            if let (Some(return_fields), Some(response)) = (&imut.return_fields, &bound.response) {
                for (field, ty) in return_fields {
                    let satisfied = response.get(field).and_then(ExprType::as_scalar) == Some(ty);
                    if !satisfied {
                        return Err(ModuleError::InterfaceContract(
                            contract.name.clone(),
                            format!(
                                "the bound mutation `{}` response does not provide field `{field}` with \
                                 the declared type",
                                imut.name
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// §13.2/§13.3: a `$modules` space exists only where its containing row is live
    /// in root state, so an install targets an existing space only when that row is
    /// present. When the root package declares the `$modules` space at this space's
    /// declaration path (`companies.…​.modules`), the containing row (`/companies/acme`)
    /// MUST be a live root row; a ghost-row space (`/companies/ghost/modules`) has no
    /// space to install into and is rejected with [`ModuleError::MissingContainingRow`].
    /// A space the root package declares no `$modules` mount for imposes no
    /// containing-row requirement — the same documented §13.2 undeclared-space seam
    /// [`check_interface_contracts`](Self::check_interface_contracts) already tolerates
    /// (an interface-less space contributes no contract either), so a host wrapping a
    /// root that does not model this mount is unaffected.
    fn check_containing_row(&self, space: &ModuleSpace) -> Result<(), ModuleError> {
        if self.root.module_space_interfaces(&space.declaration_path()).is_none() {
            return Ok(());
        }
        let present = match space.containing_row_steps() {
            Some(steps) => self.root.contains_row(&steps)?,
            None => false,
        };
        if present {
            Ok(())
        } else {
            Err(ModuleError::MissingContainingRow(space.as_str().to_owned()))
        }
    }

    /// The evaluation-ready imports a child reads through `#handle`: its §13.4
    /// parent surfaces plus its §13.5 resolved peers, each bound to its live value
    /// and type. Recomputed per read so a later parent mutation (§13.4 row-local) or
    /// a change in a bound peer's interface state is reflected.
    fn child_imports(
        &self,
        space: &ModuleSpace,
        bindings: &AdmittedBindings,
        resolved_peers: &[ResolvedPeer],
    ) -> Result<ParentImports, ModuleError> {
        let mut imports = self.parent_imports(space, bindings)?;
        for peer in resolved_peers {
            self.bind_peer(space, peer, &mut imports)?;
        }
        Ok(imports)
    }

    /// Resolve the §13.4 parent surfaces a child imports (`company: "$parent"`,
    /// `org: "$parent.company"`) into a [`ParentImports`], each handle bound to its
    /// parent surface projected row-local against the space's containing row from the
    /// root engine's current state. A `$use` handle that is not a parent capability
    /// (a sibling path or peer spec) contributes no import; a surface the space does
    /// not declare, or a missing containing row, binds nothing (the child's `#handle`
    /// read then faults, §6.3).
    fn parent_imports(&self, space: &ModuleSpace, bindings: &AdmittedBindings) -> Result<ParentImports, ModuleError> {
        let mut imports = ParentImports::default();
        let declaration = space.declaration_path();
        let steps = space.containing_row_steps().unwrap_or_default();
        for (handle, spec, _optional) in &bindings.uses {
            let surface = match spec {
                UseSpec::Parent => handle.as_str(),
                UseSpec::ParentSurface(name) => name.as_str(),
                UseSpec::Path(_) | UseSpec::Peer { .. } => continue,
            };
            if let Some(resolved) = self.root.parent_surface_projection(&declaration, &steps, surface)? {
                imports.bind(handle.clone(), resolved.ty, resolved.value);
            }
        }
        Ok(imports)
    }

    /// Bind one resolved §13.5 peer handle into `imports`. A required peer binds its
    /// sibling's exposed interface `$view` — the row-shaped `#people { id, name }`
    /// contract usage sites define (§13.5) — as a live [`Cell::Collection`] typed
    /// against the sibling's exposed view type. An optional peer binds a §13.7
    /// presence value: `true` when the handle is bound to an enabled compatible
    /// instance and `none` when it is absent, so `has(#billing)` reflects the binding
    /// rather than the interface's row count (a present-but-empty peer is still
    /// present). A required peer whose recorded sibling is no longer live binds
    /// nothing, so its `#handle` read faults (§6.3).
    fn bind_peer(
        &self,
        space: &ModuleSpace,
        peer: &ResolvedPeer,
        imports: &mut ParentImports,
    ) -> Result<(), ModuleError> {
        let live = peer
            .instance
            .as_deref()
            .and_then(|name| self.find(space, name))
            .filter(|sibling| sibling.enabled);
        if peer.optional {
            let present = live.is_some();
            let value = if present { Value::Bool(true) } else { Value::None };
            imports.bind(peer.handle.clone(), ExprType::scalar(Type::Bool), Cell::Scalar(value));
            return Ok(());
        }
        let Some(sibling) = live else {
            return Ok(());
        };
        let sibling_imports = self.parent_imports(&sibling.space, &sibling.bindings)?;
        let ty = sibling.engine.exposed_view_type(&peer.interface);
        let value = sibling.engine.interface_collection(&peer.interface, &sibling_imports).map_err(ModuleError::Engine)?;
        if let (Some(ty), Some(value)) = (ty, value) {
            imports.bind(peer.handle.clone(), ty, value);
        }
        Ok(())
    }

    /// The enabled sibling instances in `space`, reduced to the interface facts §13.5
    /// peer resolution matches against (§13.12: a disabled sibling exposes no peer
    /// availability, so it is omitted). Visited in installation order.
    fn siblings(&self, space: &ModuleSpace) -> Vec<SiblingInterface> {
        self.children
            .iter()
            .filter(|child| &child.space == space && child.enabled)
            .map(|child| {
                let (line, major) = child.engine.package_line_major();
                SiblingInterface {
                    name: child.name.clone(),
                    line: line.to_owned(),
                    major,
                    interfaces: child.engine.exposed_interface_names().map(str::to_owned).collect(),
                }
            })
            .collect()
    }

    fn find(&self, space: &ModuleSpace, name: &str) -> Option<&Child<F::Store>> {
        self.children.iter().find(|child| child.is(space, name))
    }

    fn child_mut(&mut self, space: &ModuleSpace, name: &str) -> Result<&mut Child<F::Store>, ModuleError> {
        self.children
            .iter_mut()
            .find(|child| child.is(space, name))
            .ok_or_else(|| ModuleError::Unknown(name.to_owned()))
    }

    fn enabled_child(&self, space: &ModuleSpace, name: &str) -> Result<&Child<F::Store>, ModuleError> {
        let child = self.find(space, name).ok_or_else(|| ModuleError::Unknown(name.to_owned()))?;
        if child.enabled {
            Ok(child)
        } else {
            Err(ModuleError::Disabled(name.to_owned()))
        }
    }

    fn enabled_child_mut(&mut self, space: &ModuleSpace, name: &str) -> Result<&mut Child<F::Store>, ModuleError> {
        let child = self.child_mut(space, name)?;
        if child.enabled {
            Ok(child)
        } else {
            Err(ModuleError::Disabled(name.to_owned()))
        }
    }

    fn mint_incarnation(&mut self, space: &ModuleSpace, name: &str) -> InstanceId {
        let token = format!(
            "{}#m{}-{}-{name}",
            self.root.instance().as_str(),
            self.next_incarnation,
            space.as_str().trim_start_matches('/').replace('/', "."),
        );
        self.next_incarnation += 1;
        InstanceId::new(token)
    }
}

/// The `major.minor.patch` version string of a package model (§13.15 `$from`/`$to`).
fn version_string(model: &liasse_model::Model) -> String {
    let version = &model.header().identity.version;
    format!("{}.{}.{}", version.major, version.minor, version.patch)
}
