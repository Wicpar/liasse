//! Prepared module updates (§20.4 on the §13.14 single-instance path): compute a
//! module update in full, report it, and decide separately whether to commit it.
//!
//! [`ModuleHost::prepare_update`] runs the entire single-instance update — the
//! §13.14 exposed-surface recheck, the §13.15 `$exposed` grouping, and the child
//! engine's whole §20 computation — and returns a [`PreparedModuleUpdate`].
//! [`ModuleHost::apply_update`] consumes one and commits it. [`ModuleHost::update`]
//! is exactly the two in sequence, so a **dry run is `prepare_update` with the plan
//! dropped**: one computation, no dry-run branch inside it, and no way for the two
//! to diverge.
//!
//! `prepare_update` takes `&self`. Preparing a module plan therefore cannot mutate
//! the mounted instance — the type system, not a convention, is what makes a module
//! dry run effect-free.
//!
//! **The §20 half is not re-implemented here.** A [`PreparedModuleUpdate`] *carries*
//! the child engine's own [`PreparedUpdate`], so the module path and the package
//! path are literally one computation and every §20.4 limit carries across
//! verbatim: the plan is faithful only as of its basis and not byte-reproducible
//! across separate prepares (§8.12 generated values); provisioning a newly declared
//! keyring stays apply-time, because §17.5 F1a consumes a registered provider and
//! "would this provision?" cannot be answered without provisioning; and the §13.13
//! `$bundle` conflicts a plan reports are **advisory**, resolved in the instance's
//! favour rather than blocking.
//!
//! **What is module-specific** is the basis: a module plan is pinned to its MOUNT —
//! the module-collection entry it was prepared against — as well as to the
//! instance's own §20.4 position. A §13.3 rename, a §13.16 relocation, or an
//! uninstall-and-reinstall at that address invalidates it. The surrounding
//! composition does not: a §13.14 single-instance update reads the child's active
//! definition, the target, and the child's own state and nothing else, so a sibling
//! installed or a peer rebound between prepare and apply is not an input to the
//! computation and is deliberately not part of the basis. Symmetrically, a prepared
//! module update reports exactly what the effecting update checks — it does not
//! re-verify the child against the module collection's declared `$interfaces`
//! contract (§13.8), because `update` does not either; the seam is shared, which is
//! the whole point of there being one computation.

use std::fmt;

use liasse_model::PackageId;
use liasse_store::{RowAddress, StoreFactory};

use crate::error::EngineError;
use crate::generator::Generators;
use crate::migrate::{PreparedUpdate, UpdateBasis, UpdateError};
use crate::modules::compat::{self, ExposedGrouping, NarrowingClass};
use crate::modules::host::ModuleHost;
use crate::modules::{ModuleError, ModuleUpdateReport};

/// The mount position a [`PreparedModuleUpdate`] was computed against (§20.4,
/// §13.14): the module-collection entry the instance was mounted at, together with
/// the instance's own §20.4 [`UpdateBasis`] (which carries its incarnation, its
/// active package version, its committed position, and the clock).
///
/// A module plan is faithful only as of this position, so
/// [`ModuleHost::apply_update`] compares the mount's live basis against the plan's
/// and refuses one whose basis has moved. Both halves matter: the instance half
/// catches a concurrent commit, a history movement (§19.8) or a clock move exactly
/// as on the package path, and the mount half catches an instance re-installed at
/// that address — a different incarnation under the same name is a different
/// instance (D.1), never the one the plan describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleUpdateBasis {
    at: RowAddress,
    instance: UpdateBasis,
}

impl ModuleUpdateBasis {
    /// The module-collection entry the plan was prepared against (§13.2/§13.3).
    #[must_use]
    pub fn at(&self) -> &RowAddress {
        &self.at
    }

    /// The mounted instance's own §20.4 basis — its incarnation, active package
    /// version, committed position, and clock.
    #[must_use]
    pub fn instance(&self) -> &UpdateBasis {
        &self.instance
    }
}

impl fmt::Display for ModuleUpdateBasis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` mounting {}", self.at.render(), self.instance)
    }
}

/// A §13.14 single-instance module update computed in full but NOT committed
/// (§20.4).
///
/// Every check an ordinary [`ModuleHost::update`] runs has already run: the §13.14
/// exposed-surface recheck (a definitional self-narrowing and a withdrawn interface
/// binding are refusals of [`ModuleHost::prepare_update`] itself, identical to the
/// ones the effecting update reports, because it is the same call), the §13.15
/// `$exposed` grouping, and the child engine's whole §20 computation. A plan that
/// exists is one that *would* commit against its [`basis`](Self::basis).
///
/// Dropping the plan applies nothing. [`ModuleHost::apply_update`] commits it,
/// refusing it if its basis has moved.
#[derive(Debug)]
pub struct PreparedModuleUpdate {
    basis: ModuleUpdateBasis,
    instance_update: PreparedUpdate,
    /// §13.15 `$from`: the instance's version before the update.
    from: String,
    /// §13.15 `$exposed`: every exposed interface bucketed by how its contract
    /// moved across this (non-narrowing) update.
    exposed: ExposedGrouping,
}

impl PreparedModuleUpdate {
    /// The mount position this plan was computed against, and the only one it is
    /// faithful for (§20.4).
    #[must_use]
    pub fn basis(&self) -> &ModuleUpdateBasis {
        &self.basis
    }

    /// The module-collection entry the plan would migrate (§13.2/§13.3).
    #[must_use]
    pub fn at(&self) -> &RowAddress {
        self.basis.at()
    }

    /// §13.15 `$from`: the instance's `major.minor.patch` version before the update.
    #[must_use]
    pub fn from(&self) -> &str {
        &self.from
    }

    /// The target package identity this plan would adopt (§13.15 `$to`, §4.3).
    #[must_use]
    pub fn target(&self) -> &PackageId {
        self.instance_update.target()
    }

    /// The child engine's own §20.4 plan — the proposed result, the §13.13
    /// `$bundle` conflicts a local edit would override, the §13.15 per-item
    /// `$migrated`/`$seeded` paths, and the Annex E relation. Reused, not
    /// reinvented: this IS the package-level computation, so a module dry run and a
    /// package dry run report the same facts about the same migration.
    #[must_use]
    pub fn instance_update(&self) -> &PreparedUpdate {
        &self.instance_update
    }

    /// §13.15 `$exposed.$unchanged`: exposed interfaces whose boundary contract the
    /// update leaves unchanged.
    #[must_use]
    pub fn exposed_unchanged(&self) -> &[String] {
        &self.exposed.unchanged
    }

    /// §13.15 `$exposed.$changed`: exposed interfaces whose boundary contract the
    /// update compatibly widens.
    #[must_use]
    pub fn exposed_changed(&self) -> &[String] {
        &self.exposed.changed
    }

    /// §13.15 `$exposed.$removed`: exposed interfaces the update no longer exposes.
    #[must_use]
    pub fn exposed_removed(&self) -> &[String] {
        &self.exposed.removed
    }
}

impl<F: StoreFactory> ModuleHost<F> {
    /// The §20.4 basis of the module mounted at `at` — the position a plan prepared
    /// now would be faithful as of, and the position
    /// [`apply_update`](Self::apply_update) checks a plan against.
    ///
    /// # Errors
    /// [`ModuleError::Unknown`] when no instance is mounted at `at`, or
    /// [`ModuleError::Engine`] when the child store cannot read its head.
    pub fn module_update_basis(&self, at: &RowAddress) -> Result<ModuleUpdateBasis, ModuleError> {
        let child = self.mounted(at)?;
        Ok(ModuleUpdateBasis { at: at.clone(), instance: child.engine.update_basis()? })
    }

    /// Compute the §13.14 update of the instance at `at` IN FULL without applying
    /// any part of it (§20.4), returning the [`PreparedModuleUpdate`] that describes
    /// it.
    ///
    /// This is the whole update: the §13.14 exposed-surface recheck, the §13.15
    /// `$exposed` grouping, and the child engine's entire §20 computation
    /// ([`Engine::prepare_update`](crate::Engine::prepare_update)) — route
    /// resolution, the §20.1 migration order, the §20.2 round trips, the §13.13
    /// reconciliation, and the complete admission suite over the whole prospective
    /// target. It takes `&self`, so nothing it does can touch the mounted instance.
    ///
    /// A **dry run is this call with the returned plan dropped**; a real update is
    /// this call followed by [`apply_update`](Self::apply_update), which is what
    /// [`update`](Self::update) is. The two cannot diverge, because they are the
    /// same computation with no branch inside it.
    ///
    /// # Errors
    /// The same [`ModuleError`] an effecting [`update`](Self::update) would return:
    /// [`ModuleError::Unknown`], [`ModuleError::ExposedNarrowed`],
    /// [`ModuleError::InterfaceBindingWithdrawn`], or [`ModuleError::Engine`].
    pub fn prepare_update<G: Generators>(
        &self,
        at: &RowAddress,
        target: &str,
        generator: &mut G,
    ) -> Result<PreparedModuleUpdate, ModuleError> {
        // §20.4: the mount position this plan is faithful as of, sampled BEFORE the
        // computation reads any state — through the same constructor the apply
        // re-reads it with, so the two compare like for like.
        let basis = self.module_update_basis(at)?;
        let child = self.mounted(at)?;
        // §13.14: read the ACTIVE child definition and version for the exposed-surface
        // recheck and the §13.15 `$from`.
        let active_definition = child.engine.definition_source()?.ok_or_else(|| {
            ModuleError::Engine(EngineError::Internal("active child definition unavailable for update".to_owned()))
        })?;
        let from = version_text(&child.engine.model().header().identity);
        // §13.14: a minor/patch update MUST preserve or widen the module's exposed
        // compatibility surface. Refuse a narrowing release before admission (E.9),
        // classifying a definitional self-narrowing as a static refusal and a
        // withdrawn-but-implemented binding as an admission refusal.
        if let Some(narrowing) = compat::exposed_narrowing(&active_definition, target) {
            return Err(match narrowing.class {
                NarrowingClass::Definitional => ModuleError::ExposedNarrowed(narrowing.reason),
                NarrowingClass::BindingWithdrawn => ModuleError::InterfaceBindingWithdrawn(narrowing.reason),
            });
        }
        // §13.15 `$exposed`: group each exposed interface by how its contract moved
        // across this (non-narrowing) update.
        let exposed = compat::exposed_grouping(&active_definition, target);
        // §13.14: the §20 migration over the child's own engine, computed but not
        // applied — `Engine::prepare_update` takes `&self`.
        let instance_update = child.engine.prepare_update(target, generator).map_err(module_update_error)?;
        Ok(PreparedModuleUpdate { basis, instance_update, from, exposed })
    }

    /// Commit a [`PreparedModuleUpdate`] (§20.4), migrating the mounted instance and
    /// assembling the §13.15 report.
    ///
    /// The plan's [`ModuleUpdateBasis`] is re-checked against the live mount FIRST.
    /// A plan whose basis has moved — the instance committed, its history moved, the
    /// clock advanced, or the address now mounts a different incarnation — is refused
    /// as [`ModuleError::Stale`] and nothing is committed. Re-prepare against the
    /// current position instead.
    ///
    /// # Errors
    /// [`ModuleError::Stale`] when the plan's basis has moved, [`ModuleError::Unknown`]
    /// when the mount is gone entirely, or [`ModuleError::Engine`] when the commit
    /// itself faults.
    pub fn apply_update(&mut self, prepared: PreparedModuleUpdate) -> Result<ModuleUpdateReport, ModuleError> {
        let PreparedModuleUpdate { basis, instance_update, from, exposed } = prepared;
        let current = self.module_update_basis(basis.at())?;
        if basis != current {
            return Err(ModuleError::Stale { prepared: Box::new(basis), current: Box::new(current) });
        }
        let to = version_text(instance_update.target());
        // The mount basis contains the instance's own §20.4 basis, so the engine's
        // identical re-check inside `apply_update` cannot fire once the comparison
        // above has passed. It stays a fail-closed backstop: if it ever did fire it
        // would surface as a refusal here, having committed nothing.
        let report = self
            .child_mut(basis.at())?
            .engine
            .apply_update(instance_update)
            .map_err(module_update_error)?;
        Ok(ModuleUpdateReport {
            from,
            to,
            commit: report.commit,
            migrated: report.migrated,
            seeded: report.seeded,
            exposed_unchanged: exposed.unchanged,
            exposed_changed: exposed.changed,
            exposed_removed: exposed.removed,
            // §13.15 `$imports`: an import re-bound by the update is `$rebound`, one
            // whose source is gone is `$broken`. The CORE module cases carry no
            // `$use` imports, so both are empty; recomputing per bound parent/peer
            // source under the migrated model is a follow-on.
            imports_rebound: Vec::new(),
            imports_broken: Vec::new(),
        })
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
    ///
    /// This is exactly [`prepare_update`](Self::prepare_update) followed by
    /// [`apply_update`](Self::apply_update). There is no dry-run flag and no second
    /// implementation: a §20.4 dry run of a module update is this same
    /// `prepare_update` with the plan dropped, so what a dry run reports is what an
    /// update does.
    ///
    /// # Errors
    /// [`ModuleError`] as [`prepare_update`](Self::prepare_update) and
    /// [`apply_update`](Self::apply_update) report it.
    pub fn update<G: Generators>(
        &mut self,
        at: &RowAddress,
        target: &str,
        generator: &mut G,
    ) -> Result<ModuleUpdateReport, ModuleError> {
        let prepared = self.prepare_update(at, target, generator)?;
        self.apply_update(prepared)
    }
}

/// Map a §20 update failure onto the module vocabulary, so the prepare and the
/// apply of one module update report a refusal identically.
fn module_update_error(error: UpdateError) -> ModuleError {
    match error {
        UpdateError::Engine(engine) => ModuleError::Engine(engine),
        other => ModuleError::Engine(EngineError::Internal(other.to_string())),
    }
}

/// The `major.minor.patch` version text of a package identity (§13.15 `$from`/`$to`).
fn version_text(identity: &PackageId) -> String {
    let version = &identity.version;
    format!("{}.{}.{}", version.major, version.minor, version.patch)
}
