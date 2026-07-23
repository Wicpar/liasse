//! Module lifecycle as driver-facing host operations (SPEC.md §13).
//!
//! The runtime [`ModuleHost`] owns a root [`Engine`](liasse_runtime::Engine) and
//! the child instances installed in its **row-scoped module spaces**
//! ([`ModuleSpace`], e.g. `/companies/acme/modules`), each an independently loaded
//! engine over a store the host's [`StoreFactory`](liasse_store::StoreFactory)
//! mints — that is the whole §13.3 isolation model. Its lifecycle operations
//! thread a [`Generators`](liasse_runtime) seam for the seeds an install or update
//! rolls and for a child mutation's generated `uuid()`.
//!
//! [`ModuleDeployment`] bundles that host with a single owned [`VirtualClock`] and
//! an [`Entropy`] source, so a driver runs
//! `install`/`enable`/`disable`/`uninstall`/`rename`/`update`, the
//! `child_call`/`interface_call` mutation admissions, and the interface-addressed
//! reads (`interface_read`/`aggregate`) over a space without threading a generator,
//! and returns the §13.3 rejections
//! (`EmptyName`/`DuplicateName`/`Unknown`/`Disabled`/`InvalidSpace`/
//! `MissingContainingRow`/`InvalidBinding`) as [`ModuleObservation`]s rather than
//! errors — mirroring how
//! the surface layer treats every spec refusal as a successful observation,
//! reserving [`ModuleFault`] for a genuine store/engine fault. The clock is the
//! children's request-fixed `now()` source (Annex A.5); the [`Entropy`] source is
//! the seed behind every module-minted `uuid()` (§5.1/§8.12), CSPRNG in production
//! so a module token is unpredictable — the clock never seeds a generated value.

use liasse_ident::InstanceId;
use liasse_runtime::{
    AdmittedBindings, CallOutcome, CallRequest, Engine, InstallRequest, InterfaceRow, ModuleError,
    ModuleHost, ModuleSpace, ModuleUpdateReport, ViewQuery, ViewResult,
};
use liasse_store::StoreFactory;

use crate::clock::VirtualClock;
use crate::entropy::Entropy;

/// The result of a §13.3 lifecycle operation that either applies or is refused by
/// a module-space invariant. A refusal is a successful observation, not a fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleObservation {
    /// The operation applied.
    Applied,
    /// The instance name is empty (§13.3).
    EmptyName,
    /// The instance name already names a live instance in this space (§13.3).
    DuplicateName(String),
    /// No installed instance of that name (§13.3).
    Unknown(String),
    /// The addressed instance is disabled, so its surfaces are unavailable
    /// (§13.3, §13.12).
    Disabled(String),
    /// The mount path is not a well-formed module-space location (§13.2).
    InvalidSpace(String),
    /// The module space's containing row is not live in root state, so the space
    /// does not exist and there is nothing to install into (§13.2/§13.3).
    MissingContainingRow(String),
    /// A `$use`/`$deps` binding spec is malformed (§13.5/§13.6).
    InvalidBinding(String),
    /// A required peer `$use` handle could not be resolved against the sibling set at
    /// install (§13.5): zero/several/incompatible/disabled candidates, or an explicit
    /// binding naming a cross-space instance. An admission refusal, not a fault.
    PeerUnresolved(String),
}

impl ModuleObservation {
    /// Classify a lifecycle result: `Ok` applied, a module-space rejection is an
    /// observation, and only an engine/store fault escapes as a [`ModuleFault`].
    fn of(result: Result<(), ModuleError>) -> Result<Self, ModuleFault> {
        match result {
            Ok(()) => Ok(Self::Applied),
            Err(error) => Self::refusal(error),
        }
    }

    /// Map a §13.3 module-space refusal to its observation; only an engine/store
    /// fault escapes as a [`ModuleFault`].
    fn refusal(error: ModuleError) -> Result<Self, ModuleFault> {
        match error {
            ModuleError::EmptyName => Ok(Self::EmptyName),
            ModuleError::DuplicateName(name) => Ok(Self::DuplicateName(name)),
            ModuleError::Unknown(name) => Ok(Self::Unknown(name)),
            ModuleError::Disabled(name) => Ok(Self::Disabled(name)),
            ModuleError::InvalidSpace(path) => Ok(Self::InvalidSpace(path)),
            ModuleError::MissingContainingRow(path) => Ok(Self::MissingContainingRow(path)),
            ModuleError::InvalidBinding(spec) => Ok(Self::InvalidBinding(spec)),
            ModuleError::PeerUnresolved(handle, _reason) => Ok(Self::PeerUnresolved(handle)),
            // §13.8/§13.1: a contract-satisfaction or `$config`-type refusal is a
            // static `invalid` (§13.3 "Loading validates ... before the instance
            // becomes active"), but the `ModuleObservation` vocabulary does not yet
            // model those distinct outcomes. Until the outcome enum (and the harness
            // that matches it exhaustively) grows a case, they surface as a
            // [`ModuleFault`]; a driver still classifies that as `invalid`. Giving
            // each its own first-class observation is a surface seam.
            // The §13.14 update-narrowing refusals never reach this §13.3 lifecycle
            // mapping — they arise only on the [`ModuleDeployment::update`] path,
            // classified there — so if one somehow does it is a fault, not a
            // lifecycle observation.
            fault @ (ModuleError::InterfaceContract(..)
            | ModuleError::ConfigMismatch(_)
            | ModuleError::ExposedNarrowed(_)
            | ModuleError::InterfaceBindingWithdrawn(_)
            | ModuleError::Engine(_)) => Err(ModuleFault(fault)),
        }
    }
}

/// The result of a §13.14 single-instance update.
#[derive(Debug)]
pub enum ModuleUpdate {
    /// The update migrated and committed, carrying the assembled §13.15 report.
    Updated(ModuleUpdateReport),
    /// No installed instance of that name (§13.3).
    Unknown(String),
    /// The addressed instance is disabled (§13.3, §13.12).
    Disabled(String),
    /// The update definitionally narrows the module's own exposed compatibility
    /// surface (§13.14) — a static "package loading" refusal (`invalid`): the
    /// current release stays active (E.9).
    Narrowed(String),
    /// The update withdraws a previously accepted interface binding whose private
    /// implementation remains (§13.14) — an admission refusal (`rejected`): the
    /// current binding stays active (E.9).
    Rejected(String),
}

/// A genuine store/engine fault from a module lifecycle operation — never a spec
/// outcome. A duplicate name, unknown instance, disabled instance, malformed space
/// or binding is returned as a [`ModuleObservation`]; only a broken store or a
/// failed child load is a [`ModuleFault`].
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct ModuleFault(ModuleError);

/// A root application together with the module instances installed in its
/// row-scoped module spaces, driven over a single owned virtual clock (§13).
///
/// Every module transition the deployment admits — an install/update genesis or
/// migration, a §13.11 direct-surface `child_call`, a §13.10 interface-routed
/// `interface_call` — draws its generated-value seeds from an [`Entropy`] source
/// (§5.1/§8.12), exactly as the base [`SurfaceHost`](crate::SurfaceHost) does. The
/// clock stays the request-fixed `now()` source (Annex A.5) and never seeds a
/// `uuid()`, so a module-minted token is unpredictable. Production defaults to the
/// OS CSPRNG ([`Entropy::os`]); a conformance harness injects a deterministic source
/// through [`with_entropy`](Self::with_entropy).
pub struct ModuleDeployment<F: StoreFactory> {
    host: ModuleHost<F>,
    clock: VirtualClock,
    entropy: Entropy,
}

impl<F: StoreFactory> ModuleDeployment<F> {
    /// Wrap a module `host` driven by `clock`, seeding every module transition's
    /// generated values from the OS CSPRNG (§5.1/§8.12: a module-minted `uuid()`
    /// token is unpredictable by default). A deterministic harness overrides the
    /// source with [`with_entropy`](Self::with_entropy).
    #[must_use]
    pub fn new(host: ModuleHost<F>, clock: VirtualClock) -> Self {
        Self { host, clock, entropy: Entropy::os() }
    }

    /// Replace the admission entropy source (§5.1/§8.12) — the injection seam a
    /// deterministic conformance harness uses to pin module-minted `uuid()` values
    /// reproducibly, mirroring [`SurfaceHost::with_entropy`](crate::SurfaceHost::with_entropy).
    /// A production deployment keeps the [`Entropy::os`] default.
    #[must_use]
    pub fn with_entropy(mut self, entropy: Entropy) -> Self {
        self.entropy = entropy;
        self
    }

    /// The virtual clock, for advancing time and reading the instant.
    pub fn clock_mut(&mut self) -> &mut VirtualClock {
        &mut self.clock
    }

    /// The root application engine, for reading committed state and views.
    #[must_use]
    pub fn root(&self) -> &Engine<F::Store> {
        self.host.root()
    }

    /// The root application engine, mutably (to admit root requests).
    pub fn root_mut(&mut self) -> &mut Engine<F::Store> {
        self.host.root_mut()
    }

    /// Install a new instance into `space` from an install `request` (§13.3),
    /// admitting its `$config`/`$use`/`$deps` boundary bindings: mint a fresh
    /// incarnation, create the child's private store, and load its engine (applying
    /// its own `$data` seed). An empty/duplicate name, malformed space or binding is
    /// a [`ModuleObservation`], not a fault.
    ///
    /// # Errors
    /// [`ModuleFault`] if the child store could not be created or its definition
    /// did not load.
    pub fn install(
        &mut self,
        space: &ModuleSpace,
        request: InstallRequest,
    ) -> Result<ModuleObservation, ModuleFault> {
        let now = self.clock.instant();
        let mut generators = self.entropy.generators(now);
        match self.host.install(space, request, &mut generators) {
            Ok(_incarnation) => Ok(ModuleObservation::Applied),
            Err(error) => ModuleObservation::refusal(error),
        }
    }

    /// Disable an instance (§13.3, §13.12): remove its active boundary occurrences
    /// while retaining its private stored state and history.
    ///
    /// # Errors
    /// [`ModuleFault`] on an engine/store fault.
    pub fn disable(&mut self, space: &ModuleSpace, name: &str) -> Result<ModuleObservation, ModuleFault> {
        ModuleObservation::of(self.host.disable(space, name))
    }

    /// Enable a disabled instance (§13.3): restore its boundary over the exact
    /// preserved private state.
    ///
    /// # Errors
    /// [`ModuleFault`] on an engine/store fault.
    pub fn enable(&mut self, space: &ModuleSpace, name: &str) -> Result<ModuleObservation, ModuleFault> {
        ModuleObservation::of(self.host.enable(space, name))
    }

    /// Uninstall an instance and its owned subtree (§13.3, §13.12).
    ///
    /// # Errors
    /// [`ModuleFault`] on an engine/store fault.
    pub fn uninstall(&mut self, space: &ModuleSpace, name: &str) -> Result<ModuleObservation, ModuleFault> {
        ModuleObservation::of(self.host.uninstall(space, name))
    }

    /// Rename an instance within its space (§13.3): a rekey that preserves the
    /// incarnation and therefore the durable identity (D.1). Rejects a name already
    /// in use.
    ///
    /// # Errors
    /// [`ModuleFault`] on an engine/store fault.
    pub fn rename(&mut self, space: &ModuleSpace, from: &str, to: &str) -> Result<ModuleObservation, ModuleFault> {
        ModuleObservation::of(self.host.rename(space, from, to))
    }

    /// Update a single instance in `space` to a `target` definition (§13.14/§13.15):
    /// rechecks the target's exposed compatibility surface, then runs the §20
    /// migration over the child's own engine, affecting that instance only. A
    /// successful update carries the assembled §13.15 report; a §13.14 narrowing
    /// refusal is a [`ModuleUpdate`] observation (the current release stays active,
    /// E.9), not a fault.
    ///
    /// # Errors
    /// [`ModuleFault`] only for a genuine engine/store fault while migrating.
    pub fn update(&mut self, space: &ModuleSpace, name: &str, target: &str) -> Result<ModuleUpdate, ModuleFault> {
        let now = self.clock.instant();
        let mut generators = self.entropy.generators(now);
        match self.host.update(space, name, target, &mut generators) {
            Ok(report) => Ok(ModuleUpdate::Updated(report)),
            Err(ModuleError::Unknown(name)) => Ok(ModuleUpdate::Unknown(name)),
            Err(ModuleError::Disabled(name)) => Ok(ModuleUpdate::Disabled(name)),
            // §13.14: a definitional exposed-surface narrowing is a static
            // "package loading" refusal (`invalid`); a withdrawn-but-implemented
            // interface binding is an admission refusal (`rejected`). Both are spec
            // observations, not faults.
            Err(ModuleError::ExposedNarrowed(reason)) => Ok(ModuleUpdate::Narrowed(reason)),
            Err(ModuleError::InterfaceBindingWithdrawn(reason)) => Ok(ModuleUpdate::Rejected(reason)),
            Err(fault) => Err(ModuleFault(fault)),
        }
    }

    /// Read an enabled child instance's exposed interface `$view` through the
    /// boundary (§13.8): only the projected fields cross, so a private field is
    /// unreachable here. `None` when the child declares no readable interface of
    /// that name.
    ///
    /// # Errors
    /// [`ModuleError`] if the instance is unknown, disabled, or a store fault
    /// occurred.
    pub fn interface_read(
        &self,
        space: &ModuleSpace,
        name: &str,
        interface: &str,
    ) -> Result<Option<ViewResult>, ModuleError> {
        self.host.interface_read(space, name, interface)
    }

    /// Aggregate one exposed interface across every enabled instance in `space`
    /// (§13.9). Each row carries its inherited identity (instance name + exposed
    /// row); a disabled instance is skipped (§13.12).
    ///
    /// # Errors
    /// [`ModuleError`] on a store fault.
    pub fn aggregate(&self, space: &ModuleSpace, interface: &str) -> Result<Vec<InterfaceRow>, ModuleError> {
        self.host.aggregate(space, interface)
    }

    /// Admit a mutation call against an enabled child instance (§13.11 direct
    /// module surface).
    ///
    /// # Errors
    /// [`ModuleError`] if the instance is unknown, disabled, or a store fault
    /// occurred; a rejected transition is an outcome, not an error.
    pub fn child_call(
        &mut self,
        space: &ModuleSpace,
        name: &str,
        request: &CallRequest,
    ) -> Result<CallOutcome, ModuleError> {
        let now = self.clock.instant();
        let mut generators = self.entropy.generators(now);
        self.host.child_call(space, name, request, &mut generators)
    }

    /// Evaluate a named child view at head — the §13.11 *direct* module surface,
    /// distinct from the [`ModuleDeployment::interface_read`] boundary read.
    ///
    /// # Errors
    /// [`ModuleError`] if the instance is unknown, disabled, or a store fault
    /// occurred.
    pub fn child_view(&self, space: &ModuleSpace, name: &str, view: &str) -> Result<Option<ViewResult>, ModuleError> {
        self.host.child_view(space, name, view)
    }

    /// Evaluate a **root** package view that reads its installed children through
    /// `.modules::iface` (§13.9), with the enabled instances folded into the root
    /// engine's evaluation — the aggregation a parent surface serves. Only the
    /// interface-projected fields cross the boundary (§13.8 isolation). This is the
    /// entry a `watch`/`view` on a root surface reading `.modules::iface` routes
    /// through so the installed children become visible. `None` when no view of that
    /// name is declared.
    ///
    /// # Errors
    /// [`ModuleError`] on a store or view fault while aggregating or evaluating.
    pub fn root_view(&self, name: &str, query: &ViewQuery) -> Result<Option<ViewResult>, ModuleError> {
        self.host.root_view(name, query)
    }

    /// Dispatch an interface-addressed mutation to a child's `$expose`d mutation
    /// (§13.10): route `interface.mutation` on the enabled instance in `space` to
    /// the private mutation it binds and admit it against the child atomically.
    ///
    /// # Errors
    /// [`ModuleError`] if the instance is unknown or disabled, the interface binds
    /// no such routable mutation, or a store/engine fault occurred; a rejected child
    /// transition is a [`CallOutcome`], not an error.
    pub fn interface_call(
        &mut self,
        space: &ModuleSpace,
        name: &str,
        interface: &str,
        mutation: &str,
        request: &CallRequest,
    ) -> Result<CallOutcome, ModuleError> {
        let now = self.clock.instant();
        let mut generators = self.entropy.generators(now);
        self.host.interface_call(space, name, interface, mutation, request, &mut generators)
    }

    /// Whether an instance of that name is installed in `space` (enabled or
    /// disabled).
    #[must_use]
    pub fn is_installed(&self, space: &ModuleSpace, name: &str) -> bool {
        self.host.is_installed(space, name)
    }

    /// Whether the named instance in `space` is installed and enabled.
    #[must_use]
    pub fn is_enabled(&self, space: &ModuleSpace, name: &str) -> bool {
        self.host.is_enabled(space, name)
    }

    /// The incarnation of the named instance in `space`, if installed (§13.3, D.1).
    #[must_use]
    pub fn incarnation(&self, space: &ModuleSpace, name: &str) -> Option<&InstanceId> {
        self.host.incarnation(space, name)
    }

    /// The admitted boundary bindings of the named instance in `space` (§13.3).
    #[must_use]
    pub fn bindings(&self, space: &ModuleSpace, name: &str) -> Option<&AdmittedBindings> {
        self.host.bindings(space, name)
    }
}
