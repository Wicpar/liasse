//! Module lifecycle as driver-facing host operations (SPEC.md §13).
//!
//! The runtime [`ModuleHost`] owns a root [`Engine`](liasse_runtime::Engine) and
//! the installed child instances mounted in its module spaces, each an
//! independently loaded engine over a store the host's
//! [`StoreFactory`](liasse_store::StoreFactory) mints — that is the whole §13.3
//! isolation model. Its lifecycle operations already thread a
//! [`Generators`](liasse_runtime) seam for the seeds an install or update rolls.
//!
//! [`ModuleDeployment`] bundles that host with a single owned [`VirtualClock`], so
//! a driver runs `install`/`enable`/`disable`/`uninstall`/`rename`/`update`
//! without threading a generator, and returns the §13.3 rejections
//! (`DuplicateName`/`Unknown`/`Disabled`) as [`ModuleObservation`]s rather than
//! errors — mirroring how the surface layer treats every spec refusal as a
//! successful observation, reserving [`ModuleFault`] for a genuine store/engine
//! fault. The bundled clock is the children's `now()` source and drives
//! per-instance temporal reads deterministically.

use liasse_ident::InstanceId;
use liasse_runtime::{
    CallOutcome, CallRequest, Engine, ModuleError, ModuleHost, UpdateReport, ViewResult,
};
use liasse_store::StoreFactory;

use crate::clock::VirtualClock;

/// The result of a §13.3 lifecycle operation that either applies or is refused by
/// a module-space invariant. A refusal is a successful observation, not a fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleObservation {
    /// The operation applied.
    Applied,
    /// The instance name already names a live instance in this space (§13.3).
    DuplicateName(String),
    /// No installed instance of that name (§13.3).
    Unknown(String),
    /// The addressed instance is disabled, so its surfaces are unavailable
    /// (§13.3, §13.12).
    Disabled(String),
}

impl ModuleObservation {
    /// Classify a lifecycle result: `Ok` applied, a module-space rejection is an
    /// observation, and only an engine/store fault escapes as a [`ModuleFault`].
    fn of(result: Result<(), ModuleError>) -> Result<Self, ModuleFault> {
        match result {
            Ok(()) => Ok(Self::Applied),
            Err(ModuleError::DuplicateName(name)) => Ok(Self::DuplicateName(name)),
            Err(ModuleError::Unknown(name)) => Ok(Self::Unknown(name)),
            Err(ModuleError::Disabled(name)) => Ok(Self::Disabled(name)),
            Err(fault @ ModuleError::Engine(_)) => Err(ModuleFault(fault)),
        }
    }
}

/// The result of a §13.14 single-instance update.
#[derive(Debug)]
pub enum ModuleUpdate {
    /// The update migrated and committed, with its Annex E relation and commit.
    Updated(UpdateReport),
    /// No installed instance of that name (§13.3).
    Unknown(String),
    /// The addressed instance is disabled (§13.3, §13.12).
    Disabled(String),
}

/// A genuine store/engine fault from a module lifecycle operation — never a spec
/// outcome. A duplicate name, unknown instance, or disabled instance is returned
/// as a [`ModuleObservation`]; only a broken store or a failed child load is a
/// [`ModuleFault`].
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct ModuleFault(ModuleError);

/// A root application together with its installed module instances, driven over a
/// single owned virtual clock (§13).
pub struct ModuleDeployment<F: StoreFactory> {
    host: ModuleHost<F>,
    clock: VirtualClock,
}

impl<F: StoreFactory> ModuleDeployment<F> {
    /// Wrap a module `host` driven by `clock`.
    #[must_use]
    pub fn new(host: ModuleHost<F>, clock: VirtualClock) -> Self {
        Self { host, clock }
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

    /// Install a new named child instance from a module `definition` (§13.3):
    /// mint a fresh incarnation, create the child's private store, and load its
    /// engine (applying its own `$data` seed). Rejects a duplicate name.
    ///
    /// # Errors
    /// [`ModuleFault`] if the child store could not be created or its definition
    /// did not load.
    pub fn install(&mut self, name: &str, definition: &str) -> Result<ModuleObservation, ModuleFault> {
        match self.host.install(name, definition, &mut self.clock) {
            Ok(_incarnation) => Ok(ModuleObservation::Applied),
            Err(ModuleError::DuplicateName(name)) => Ok(ModuleObservation::DuplicateName(name)),
            Err(fault) => Err(ModuleFault(fault)),
        }
    }

    /// Disable an instance (§13.3, §13.12): remove its active surfaces while
    /// retaining its private stored state and history.
    ///
    /// # Errors
    /// [`ModuleFault`] on an engine/store fault.
    pub fn disable(&mut self, name: &str) -> Result<ModuleObservation, ModuleFault> {
        ModuleObservation::of(self.host.disable(name))
    }

    /// Enable a disabled instance (§13.3): restore its surfaces over the exact
    /// preserved private state.
    ///
    /// # Errors
    /// [`ModuleFault`] on an engine/store fault.
    pub fn enable(&mut self, name: &str) -> Result<ModuleObservation, ModuleFault> {
        ModuleObservation::of(self.host.enable(name))
    }

    /// Uninstall an instance and its owned subtree (§13.3, §13.12).
    ///
    /// # Errors
    /// [`ModuleFault`] on an engine/store fault.
    pub fn uninstall(&mut self, name: &str) -> Result<ModuleObservation, ModuleFault> {
        ModuleObservation::of(self.host.uninstall(name))
    }

    /// Rename an instance (§13.3): a rekey that preserves the incarnation and
    /// therefore the durable identity (D.1). Rejects a name already in use.
    ///
    /// # Errors
    /// [`ModuleFault`] on an engine/store fault.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<ModuleObservation, ModuleFault> {
        ModuleObservation::of(self.host.rename(from, to))
    }

    /// Update a single instance to a `target` definition (§13.14): delegates to the
    /// §20 migration over the child's own engine, affecting that instance only.
    ///
    /// # Errors
    /// [`ModuleFault`] if the migration was refused by the admission pipeline or an
    /// engine/store fault occurred (the runtime host collapses a rejected migration
    /// into an engine fault — surfacing the migration [`Rejection`](liasse_runtime)
    /// as its own observation remains a runtime seam).
    pub fn update(&mut self, name: &str, target: &str) -> Result<ModuleUpdate, ModuleFault> {
        match self.host.update(name, target, &mut self.clock) {
            Ok(report) => Ok(ModuleUpdate::Updated(report)),
            Err(ModuleError::Unknown(name)) => Ok(ModuleUpdate::Unknown(name)),
            Err(ModuleError::Disabled(name)) => Ok(ModuleUpdate::Disabled(name)),
            Err(fault) => Err(ModuleFault(fault)),
        }
    }

    /// Admit a mutation call against an enabled child instance (§13.11 direct
    /// module surface).
    ///
    /// # Errors
    /// [`ModuleError`] if the instance is unknown, disabled, or a store fault
    /// occurred; a rejected transition is an outcome, not an error.
    pub fn child_call(&mut self, name: &str, request: &CallRequest) -> Result<CallOutcome, ModuleError> {
        self.host.child_call(name, request, &mut self.clock)
    }

    /// Evaluate an enabled child instance's view at its head (§13.9).
    ///
    /// # Errors
    /// [`ModuleError`] if the instance is unknown, disabled, or a store fault
    /// occurred.
    pub fn child_view(&self, name: &str, view: &str) -> Result<Option<ViewResult>, ModuleError> {
        self.host.child_view(name, view)
    }

    /// Whether an instance of that name is installed (enabled or disabled).
    #[must_use]
    pub fn is_installed(&self, name: &str) -> bool {
        self.host.is_installed(name)
    }

    /// Whether the named instance is installed and enabled.
    #[must_use]
    pub fn is_enabled(&self, name: &str) -> bool {
        self.host.is_enabled(name)
    }

    /// The incarnation of the named instance, if installed (§13.3, D.1).
    #[must_use]
    pub fn incarnation(&self, name: &str) -> Option<&InstanceId> {
        self.host.incarnation(name)
    }
}
