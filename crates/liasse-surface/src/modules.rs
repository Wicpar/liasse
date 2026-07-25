//! Module lifecycle as driver-facing host operations (SPEC.md §13).
//!
//! The runtime [`ModuleHost`] owns a root [`Engine`](liasse_runtime::Engine) and
//! the child instances mounted in its **module collections** — ordinary maps of
//! `module` values, one instance per entry — each an independently loaded engine
//! over a store the host's [`StoreFactory`](liasse_store::StoreFactory) mints,
//! which is the whole §13.3 isolation model. An instance is addressed by the
//! [`RowAddress`] of its entry: the same address every other row has, so there is
//! no module-space coordinate to construct. Its lifecycle operations thread a
//! [`Generators`](liasse_runtime) seam for the seeds an install or update rolls and
//! for a child mutation's generated `uuid()`.
//!
//! [`ModuleDeployment`] bundles that host with a single owned [`VirtualClock`] and
//! an [`Entropy`] source, so a driver runs
//! `install`/`enable`/`disable`/`uninstall`/`rename`/`update` (and the §20.4
//! `dry_run_update`, the same update computed and discarded), the
//! `child_call`/`interface_call` mutation admissions, and the interface-addressed
//! read (`interface_read`) without threading a generator, and returns the §13.3
//! rejections (`EmptyName`/`DuplicateName`/`Unknown`/`Disabled`/
//! `MissingContainingRow`/`InvalidBinding`) as [`ModuleObservation`]s rather than
//! errors — mirroring how
//! the surface layer treats every spec refusal as a successful observation,
//! reserving [`ModuleFault`] for a genuine store/engine fault. The clock is the
//! children's request-fixed `now()` source (Annex A.5); the [`Entropy`] source is
//! the seed behind every module-minted `uuid()` (§5.1/§8.12), CSPRNG in production
//! so a module token is unpredictable — the clock never seeds a generated value.

use liasse_ident::InstanceId;
use liasse_runtime::{
    AdmittedBindings, CallOutcome, CallRequest, Engine, InstallRequest, ModuleError, ModuleHost,
    ModuleUpdateReport, PreparedModuleUpdate, ViewQuery, ViewResult,
};
use liasse_store::{CollectionPath, RowAddress, StoreFactory};
use liasse_value::BlobDescriptor;

use crate::clock::VirtualClock;
use crate::entropy::Entropy;

/// The result of a §13.3 lifecycle operation that either applies or is refused by
/// a module-collection invariant. A refusal is a successful observation, not a
/// fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleObservation {
    /// The operation applied.
    Applied,
    /// The instance name is empty (§13.3).
    EmptyName,
    /// The instance name already names a live instance in this module collection
    /// (§13.3). Bad input, so it is an observation here and a `rejected` outcome on
    /// the in-language path — never an error.
    DuplicateName(String),
    /// No installed instance of that name (§13.3).
    Unknown(String),
    /// The addressed instance is disabled, so its surfaces are unavailable
    /// (§13.3, §13.12).
    Disabled(String),
    /// The module collection's containing row is not live in root state, so the
    /// collection does not exist and there is nothing to install into (§13.2/§13.3).
    /// Bad input, so it is an observation here and a `rejected` outcome on the
    /// in-language path — never an error.
    MissingContainingRow(String),
    /// A `$use`/`$deps` binding spec is malformed (§13.5/§13.6).
    InvalidBinding(String),
    /// A required peer `$use` handle could not be resolved against the sibling set at
    /// install (§13.5): zero/several/incompatible/disabled candidates, or an explicit
    /// binding naming a non-sibling instance. An admission refusal, not a fault.
    PeerUnresolved(String),
}

impl ModuleObservation {
    /// Classify a lifecycle result: `Ok` applied, a module-collection rejection is
    /// an observation, and only an engine/store fault escapes as a
    /// [`ModuleFault`].
    fn of(result: Result<(), ModuleError>) -> Result<Self, ModuleFault> {
        match result {
            Ok(()) => Ok(Self::Applied),
            Err(error) => Self::refusal(error),
        }
    }

    /// Map a §13.3 module-collection refusal to its observation; only an
    /// engine/store fault escapes as a [`ModuleFault`].
    fn refusal(error: ModuleError) -> Result<Self, ModuleFault> {
        match error {
            ModuleError::EmptyName => Ok(Self::EmptyName),
            ModuleError::DuplicateName(name) => Ok(Self::DuplicateName(name)),
            ModuleError::Unknown(name) => Ok(Self::Unknown(name)),
            ModuleError::Disabled(name) => Ok(Self::Disabled(name)),
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
            // The §13.14 update-narrowing refusals and the §20.4 stale-plan refusal
            // never reach this §13.3 lifecycle mapping — they arise only on the
            // [`ModuleDeployment::update`] path, classified there — so if one
            // somehow does it is a fault, not a lifecycle observation.
            fault @ (ModuleError::InterfaceContract(..)
            | ModuleError::ConfigMismatch(_)
            | ModuleError::ExposedNarrowed(_)
            | ModuleError::InterfaceBindingWithdrawn(_)
            | ModuleError::Stale { .. }
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

/// The observable result of a §20.4 **dry run** of a §13.14 single-instance update:
/// the same update computed in full, then discarded.
///
/// Its refusal variants are the refusal variants of [`ModuleUpdate`], produced by
/// the same computation and classified by the same function, so a dry run reports
/// the outcome the update would have. The success variant carries the
/// [`PreparedModuleUpdate`] instead of a report, because a dry run takes no commit:
/// the update is described, not applied, and the mounted instance is untouched.
#[derive(Debug)]
pub enum ModuleUpdatePreview {
    /// The update computes and every §13.14/§20.1 invariant holds; the plan
    /// describes what it would do, against the mount position it names.
    Ready(Box<PreparedModuleUpdate>),
    /// No installed instance of that name (§13.3).
    Unknown(String),
    /// The addressed instance is disabled (§13.3, §13.12).
    Disabled(String),
    /// The update definitionally narrows the module's own exposed compatibility
    /// surface (§13.14) — a static "package loading" refusal (`invalid`).
    Narrowed(String),
    /// The update withdraws a previously accepted interface binding whose private
    /// implementation remains (§13.14) — an admission refusal (`rejected`).
    Rejected(String),
}

/// A §13.14/§13.3 refusal of a single-instance update that is a spec OBSERVATION
/// rather than a fault.
///
/// The effecting [`ModuleDeployment::update`] and its §20.4
/// [`dry_run_update`](ModuleDeployment::dry_run_update) classify a refusal through
/// this one function, so the two cannot disagree about what a refusal is — the same
/// guarantee at the surface that one shared prepare gives at the runtime.
enum UpdateRefusal {
    Unknown(String),
    Disabled(String),
    Narrowed(String),
    Rejected(String),
}

impl UpdateRefusal {
    /// Classify a module update failure: a §13.3/§13.14 refusal is an observation,
    /// and only a genuine engine/store fault escapes as a [`ModuleFault`].
    fn classify(error: ModuleError) -> Result<Self, ModuleFault> {
        match error {
            ModuleError::Unknown(name) => Ok(Self::Unknown(name)),
            ModuleError::Disabled(name) => Ok(Self::Disabled(name)),
            // §13.14: a definitional exposed-surface narrowing is a static "package
            // loading" refusal (`invalid`); a withdrawn-but-implemented interface
            // binding is an admission refusal (`rejected`). Both are spec
            // observations, not faults.
            ModuleError::ExposedNarrowed(reason) => Ok(Self::Narrowed(reason)),
            ModuleError::InterfaceBindingWithdrawn(reason) => Ok(Self::Rejected(reason)),
            fault => Err(ModuleFault(fault)),
        }
    }
}

impl From<UpdateRefusal> for ModuleUpdate {
    fn from(refusal: UpdateRefusal) -> Self {
        match refusal {
            UpdateRefusal::Unknown(name) => Self::Unknown(name),
            UpdateRefusal::Disabled(name) => Self::Disabled(name),
            UpdateRefusal::Narrowed(reason) => Self::Narrowed(reason),
            UpdateRefusal::Rejected(reason) => Self::Rejected(reason),
        }
    }
}

impl From<UpdateRefusal> for ModuleUpdatePreview {
    fn from(refusal: UpdateRefusal) -> Self {
        match refusal {
            UpdateRefusal::Unknown(name) => Self::Unknown(name),
            UpdateRefusal::Disabled(name) => Self::Disabled(name),
            UpdateRefusal::Narrowed(reason) => Self::Narrowed(reason),
            UpdateRefusal::Rejected(reason) => Self::Rejected(reason),
        }
    }
}

/// A genuine store/engine fault from a module lifecycle operation — never a spec
/// outcome. A duplicate name, unknown instance, disabled instance, absent containing row
/// or binding is returned as a [`ModuleObservation`]; only a broken store or a
/// failed child load is a [`ModuleFault`].
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct ModuleFault(ModuleError);

/// A root application together with the module instances installed in its
/// row-scoped module collections, driven over a single owned virtual clock (§13).
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

    /// Install a new instance into the module collection `collection` from an
    /// install `request` (§13.3), admitting its `$config`/`$use`/`$deps` boundary
    /// bindings: mint a fresh incarnation, create the child's private store, and
    /// load its engine (applying its own `$data` seed). The instance is mounted at
    /// the collection entry keyed by the request's name. An empty/duplicate name, a
    /// containing row that is not live, or a malformed binding is a
    /// [`ModuleObservation`], not a fault.
    ///
    /// # Errors
    /// [`ModuleFault`] if the child store could not be created or its definition
    /// did not load.
    pub fn install(
        &mut self,
        collection: &CollectionPath,
        request: InstallRequest,
    ) -> Result<ModuleObservation, ModuleFault> {
        let now = self.clock.instant();
        let mut generators = self.entropy.generators(now);
        match self.host.install(collection, request, &mut generators) {
            Ok(_incarnation) => Ok(ModuleObservation::Applied),
            Err(error) => ModuleObservation::refusal(error),
        }
    }

    /// Admit a **host/root-scope** transition that carries module instances through
    /// their lifecycle (§13.10, §13.16): the root program is lent the host-privileged
    /// handle, so a `module.install`/`update`/`remove` call, a §13.16 operator, or a
    /// `<-` move into a module-collection entry stages into the very same transition
    /// as the root's own change.
    ///
    /// This is the host lifecycle entry, not a client one. §13.10 lends the privilege
    /// by SCOPE, so an external `$public` caller reaching the same mutation through
    /// [`SurfaceHost`](crate::SurfaceHost) is lent nothing and is refused. Both paths
    /// exist on purpose; a driver must not substitute one for the other.
    ///
    /// # Errors
    /// [`ModuleError`] on a store/engine fault, or on a lifecycle refusal the host
    /// reports as an error (an unreachable mount, a duplicate name); a rejected
    /// transition is a [`CallOutcome`], not an error.
    pub fn lifecycle_call(&mut self, request: &CallRequest) -> Result<CallOutcome, ModuleError> {
        let now = self.clock.instant();
        let mut generators = self.entropy.generators(now);
        self.host.call_root_lifecycle(request, &mut generators)
    }

    /// Store a `.liasse` package's bytes in the ROOT's §18.3 blob storage and return
    /// the descriptor addressing them — what a `blob`-typed argument to a §13.10 /
    /// §13.16 lifecycle mutation carries (`unpack(@package)`).
    ///
    /// # Errors
    /// [`ModuleError`] on a store fault.
    pub fn store_package_blob(
        &mut self,
        bytes: &[u8],
        name: Option<String>,
    ) -> Result<BlobDescriptor, ModuleError> {
        self.host.store_package_blob(bytes, name)
    }

    /// Disable an instance (§13.3, §13.12): remove its active boundary occurrences
    /// while retaining its private stored state and history.
    ///
    /// # Errors
    /// [`ModuleFault`] on an engine/store fault.
    pub fn disable(&mut self, at: &RowAddress) -> Result<ModuleObservation, ModuleFault> {
        ModuleObservation::of(self.host.disable(at))
    }

    /// Enable a disabled instance (§13.3): restore its boundary over the exact
    /// preserved private state.
    ///
    /// # Errors
    /// [`ModuleFault`] on an engine/store fault.
    pub fn enable(&mut self, at: &RowAddress) -> Result<ModuleObservation, ModuleFault> {
        ModuleObservation::of(self.host.enable(at))
    }

    /// Uninstall an instance and its owned subtree (§13.3, §13.12).
    ///
    /// # Errors
    /// [`ModuleFault`] on an engine/store fault.
    pub fn uninstall(&mut self, at: &RowAddress) -> Result<ModuleObservation, ModuleFault> {
        ModuleObservation::of(self.host.uninstall(at))
    }

    /// Rename an instance within its module collection (§13.3): a rekey that
    /// preserves the incarnation and therefore the durable identity (D.1). Rejects a
    /// name already in use.
    ///
    /// # Errors
    /// [`ModuleFault`] on an engine/store fault.
    pub fn rename(&mut self, at: &RowAddress, to: &str) -> Result<ModuleObservation, ModuleFault> {
        ModuleObservation::of(self.host.rename(at, to))
    }

    /// Update the instance at `at` to a `target` definition (§13.14/§13.15):
    /// rechecks the target's exposed compatibility surface, then runs the §20
    /// migration over the child's own engine, affecting that instance only. A
    /// successful update carries the assembled §13.15 report; a §13.14 narrowing
    /// refusal is a [`ModuleUpdate`] observation (the current release stays active,
    /// E.9), not a fault.
    ///
    /// # Errors
    /// [`ModuleFault`] only for a genuine engine/store fault while migrating.
    pub fn update(&mut self, at: &RowAddress, target: &str) -> Result<ModuleUpdate, ModuleFault> {
        let now = self.clock.instant();
        let mut generators = self.entropy.generators(now);
        match self.host.update(at, target, &mut generators) {
            Ok(report) => Ok(ModuleUpdate::Updated(report)),
            Err(error) => UpdateRefusal::classify(error).map(Into::into),
        }
    }

    /// Compute the §13.14 update of the instance at `at` in full WITHOUT applying it
    /// (§20.4) — the driver-facing **dry run** of a module update.
    ///
    /// This runs [`ModuleHost::prepare_update`], the very computation
    /// [`update`](Self::update) runs before it commits, and hands back the resulting
    /// plan in a [`ModuleUpdatePreview`] instead of committing it. Nothing is
    /// applied: no migration, no commit, no version movement. A refusal here is the
    /// refusal `update` would report, because it is the same call producing it.
    ///
    /// The returned plan is faithful only as of its own mount basis (§20.4); it is a
    /// description of the update, not a reservation of it.
    ///
    /// # Errors
    /// [`ModuleFault`] only for a genuine engine/store fault while preparing.
    pub fn dry_run_update(
        &mut self,
        at: &RowAddress,
        target: &str,
    ) -> Result<ModuleUpdatePreview, ModuleFault> {
        let now = self.clock.instant();
        let mut generators = self.entropy.generators(now);
        match self.host.prepare_update(at, target, &mut generators) {
            Ok(prepared) => Ok(ModuleUpdatePreview::Ready(Box::new(prepared))),
            Err(error) => UpdateRefusal::classify(error).map(Into::into),
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
        at: &RowAddress,
        interface: &str,
    ) -> Result<Option<ViewResult>, ModuleError> {
        self.host.interface_read(at, interface)
    }

    /// Admit a mutation call against an enabled child instance (§13.11 direct
    /// module surface).
    ///
    /// # Errors
    /// [`ModuleError`] if the instance is unknown, disabled, or a store fault
    /// occurred; a rejected transition is an outcome, not an error.
    pub fn child_call(&mut self, at: &RowAddress, request: &CallRequest) -> Result<CallOutcome, ModuleError> {
        let now = self.clock.instant();
        let mut generators = self.entropy.generators(now);
        self.host.child_call(at, request, &mut generators)
    }

    /// Evaluate a named child view at head — the §13.11 *direct* module surface,
    /// distinct from the [`ModuleDeployment::interface_read`] boundary read.
    ///
    /// # Errors
    /// [`ModuleError`] if the instance is unknown, disabled, or a store fault
    /// occurred.
    pub fn child_view(&self, at: &RowAddress, view: &str) -> Result<Option<ViewResult>, ModuleError> {
        self.host.child_view(at, view)
    }

    /// Evaluate a **root** package view that reads its mounted children through a
    /// module collection, with the enabled instances materialized into the root
    /// engine's evaluation. `.modules::iface` there is the §6.4 nested traversal
    /// every collection has, and only the interface-projected fields cross the
    /// boundary (§13.8 isolation). This is the entry a `watch`/`view` on such a root
    /// surface routes through so the mounted children become visible. `None` when no
    /// view of that name is declared.
    ///
    /// # Errors
    /// [`ModuleError`] on a store or view fault while aggregating or evaluating.
    pub fn root_view(&self, name: &str, query: &ViewQuery) -> Result<Option<ViewResult>, ModuleError> {
        self.host.root_view(name, query)
    }

    /// Dispatch an interface-addressed mutation to a child's `$expose`d mutation
    /// (§13.10): route `interface.mutation` on the enabled instance at `at` to the
    /// private mutation it binds and admit it against the child atomically.
    ///
    /// # Errors
    /// [`ModuleError`] if the instance is unknown or disabled, the interface binds
    /// no such routable mutation, or a store/engine fault occurred; a rejected child
    /// transition is a [`CallOutcome`], not an error.
    pub fn interface_call(
        &mut self,
        at: &RowAddress,
        interface: &str,
        mutation: &str,
        request: &CallRequest,
    ) -> Result<CallOutcome, ModuleError> {
        let now = self.clock.instant();
        let mut generators = self.entropy.generators(now);
        self.host.interface_call(at, interface, mutation, request, &mut generators)
    }

    /// Whether an instance is mounted at `at` (enabled or disabled).
    #[must_use]
    pub fn is_installed(&self, at: &RowAddress) -> bool {
        self.host.is_installed(at)
    }

    /// Whether the instance at `at` is mounted and enabled.
    #[must_use]
    pub fn is_enabled(&self, at: &RowAddress) -> bool {
        self.host.is_enabled(at)
    }

    /// The incarnation of the instance at `at`, if mounted (§13.3, D.1).
    #[must_use]
    pub fn incarnation(&self, at: &RowAddress) -> Option<&InstanceId> {
        self.host.incarnation(at)
    }

    /// The admitted boundary bindings of the instance at `at` (§13.3).
    #[must_use]
    pub fn bindings(&self, at: &RowAddress) -> Option<&AdmittedBindings> {
        self.host.bindings(at)
    }
}
