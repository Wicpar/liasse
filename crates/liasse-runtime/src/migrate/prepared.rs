//! Prepared package updates (§20.4): compute an update in full, report it, and
//! decide separately whether to commit it.
//!
//! [`Engine::prepare_update`] runs the entire §20 computation — route resolution,
//! the §20.1 migration order, the §13.13 `$bundle` reconciliation, and the complete
//! §20.1 admission suite over the whole prospective target — and returns a
//! [`PreparedUpdate`]. [`Engine::apply_update`] consumes one and commits it.
//! [`Engine::update`] is exactly the two in sequence, so a **dry run is
//! `prepare_update` with the plan dropped**: there is one computation and no
//! dry-run branch inside it, and the two cannot diverge.
//!
//! `prepare_update` takes `&self`. Preparing a plan therefore cannot mutate the
//! instance — the type system, not a convention, is what makes a dry run
//! effect-free.
//!
//! **The guarantee a prepared plan makes** is stated against its
//! [`UpdateBasis`]: *every §20.1 invariant holds, and exactly these §13.13
//! divergences exist, for this instance at this commit position and this clock*.
//! It is not a promise of byte-identical output from a later, separately prepared
//! run: generated values resolve at admission (§8.12), so re-preparing the same
//! target draws fresh `uuid()` values — and `now()` from whatever instant that
//! later prepare samples. Applying *this* plan commits the values *this* plan
//! computed, never values re-derived at apply time.

use std::fmt;

use liasse_artifact::UpdateRelation;
use liasse_ident::InstanceId;
use liasse_model::PackageId;
use liasse_store::{CommitSeq, InstanceStore};
use liasse_value::Timestamp;

use crate::engine::{Compilation, Engine};
use crate::error::EngineError;
use crate::history::MergeOutcome;

use super::{UpdateError, UpdateReport};

/// The state position a [`PreparedUpdate`] was computed against (§20.4).
///
/// A plan is faithful only as of this position. A concurrent commit, a history
/// movement (§19.8), or a clock move (§14) invalidates it, so
/// [`Engine::apply_update`] compares the instance's live basis against the plan's
/// and refuses a plan whose basis has moved rather than committing a computation
/// that no longer describes reality.
///
/// The clock is part of the basis because it is an input to the computation: a
/// prepared row may carry a `now()`-derived value, while the commit stamps every
/// inserted row's `$created` with the clock at apply time (§22.5/§22.6). Applying
/// across a clock move would bake two different instants into one transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateBasis {
    instance: InstanceId,
    package: PackageId,
    head: CommitSeq,
    clock: Timestamp,
}

impl UpdateBasis {
    /// The instance the plan was computed for (D.1). A plan never crosses
    /// instances.
    #[must_use]
    pub fn instance(&self) -> &InstanceId {
        &self.instance
    }

    /// The package identity active when the plan was computed — the migration's
    /// source version (§20.1).
    #[must_use]
    pub fn package(&self) -> &PackageId {
        &self.package
    }

    /// The store head the plan was computed over.
    #[must_use]
    pub fn head(&self) -> CommitSeq {
        self.head
    }

    /// The virtual clock (§14, A.5) the plan's `now()` samples resolved against.
    #[must_use]
    pub fn clock(&self) -> Timestamp {
        self.clock
    }
}

impl fmt::Display for UpdateBasis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "instance `{}` on `{}@{}.{}.{}` at commit {}, clock {}",
            self.instance,
            self.package.name.as_str(),
            self.package.version.major,
            self.package.version.minor,
            self.package.version.patch,
            self.head.get(),
            self.clock.count(),
        )
    }
}

/// A §20 package update computed in full but NOT committed (§20.4).
///
/// Every check an ordinary update runs has already run: the Annex-E relation and
/// route resolution, the §20.1 migration order, the reversible round trips
/// (§20.2), the §13.13 `$bundle` reconciliation, the §16.2 requirement gate, the
/// §17.6 keyring-policy gate, and the complete §20.1 admission suite over the whole
/// prospective target. A plan that exists is one that *would* commit against its
/// [`basis`](Self::basis); a rejection surfaces as the [`UpdateError`] of
/// [`Engine::prepare_update`] itself, identical to the one the effecting update
/// reports, because it is the same call.
///
/// Dropping the plan applies nothing. [`Engine::apply_update`] commits it, refusing
/// it if its basis has moved.
///
/// §19.9's reconciliation plan also carries "affected module boundaries"; a CORE
/// update prepares one instance's own boundary, and the module-space composition is
/// the documented seam there as elsewhere.
pub struct PreparedUpdate {
    /// The compiled target — the artefacts the commit adopts.
    pub(crate) compilation: Compilation,
    /// The target definition text, staged verbatim as the new active definition.
    pub(crate) definition: String,
    /// The Annex E relation of the target to the active package (§20.3).
    pub(crate) relation: UpdateRelation,
    /// The position this plan is faithful as of.
    pub(crate) basis: UpdateBasis,
    /// The §19.9-shaped reconciliation: `merged` is the complete proposed result —
    /// every row the update would leave live — and `conflicts` the §13.13
    /// coordinates both sides moved.
    pub(crate) reconciliation: MergeOutcome,
    /// §13.15 `$migrated`: the display paths a declared migration transform
    /// produced, in canonical path order.
    pub(crate) migrated: Vec<String>,
    /// §13.15 `$seeded`: the display paths the apply-if-absent seed pass inserted,
    /// in canonical path order.
    pub(crate) seeded: Vec<String>,
}

impl PreparedUpdate {
    /// The Annex E relation of the target to the active package (§20.3).
    #[must_use]
    pub fn relation(&self) -> UpdateRelation {
        self.relation
    }

    /// The state position this plan was computed against, and the only one it is
    /// faithful for (§20.4).
    #[must_use]
    pub fn basis(&self) -> &UpdateBasis {
        &self.basis
    }

    /// The target package identity this plan would adopt (§4.3).
    #[must_use]
    pub fn target(&self) -> &PackageId {
        &self.compilation.model.header().identity
    }

    /// The §19.9-shaped reconciliation this update computed.
    ///
    /// [`merged`](MergeOutcome::merged) is the **proposed result**: exactly the rows
    /// the instance would hold after the commit, keyed by row address. Comparing it
    /// with live state is what a deploy preview renders.
    ///
    /// [`conflicts`](MergeOutcome::conflicts) are the §13.13 `$bundle` coordinates
    /// where the release and the instance BOTH moved: an
    /// [`IncompatibleValue`](crate::ConflictKind::IncompatibleValue) where each side
    /// set a bundled field to a different value, a
    /// [`DeleteVsModify`](crate::ConflictKind::DeleteVsModify) where the release
    /// dropped a bundled row the instance had edited. Unlike a §19.9 merge, §13.13
    /// *resolves* every one of them — the instance's value is retained — so a
    /// reported conflict does **not** block the update. It says which
    /// package-authored values this instance's own edits will override, which is
    /// precisely what a host must show before deploying.
    #[must_use]
    pub fn reconciliation(&self) -> &MergeOutcome {
        &self.reconciliation
    }

    /// §13.15 `$migrated`: the canonical display paths of rows a declared migration
    /// transform produced, in canonical path order. A §20.1 compatible
    /// same-identity copy is not a migrated row.
    #[must_use]
    pub fn migrated(&self) -> &[String] {
        &self.migrated
    }

    /// §13.15 `$seeded`: the canonical display paths of seed rows the §13.13
    /// apply-if-absent pass would insert at an address the instance does not hold,
    /// in canonical path order.
    #[must_use]
    pub fn seeded(&self) -> &[String] {
        &self.seeded
    }
}

impl fmt::Debug for PreparedUpdate {
    /// The plan's report facts only — the compiled target artefacts are internal
    /// machinery, not part of what the plan reports.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedUpdate")
            .field("target", self.target())
            .field("relation", &self.relation)
            .field("basis", &self.basis)
            .field("rows", &self.reconciliation.merged.len())
            .field("conflicts", &self.reconciliation.conflicts)
            .field("migrated", &self.migrated)
            .field("seeded", &self.seeded)
            .finish()
    }
}

impl<S: InstanceStore> Engine<S> {
    /// This instance's current [`UpdateBasis`] (§20.4) — the position a plan
    /// prepared now would be faithful as of, and the position
    /// [`apply_update`](Self::apply_update) checks a plan against.
    ///
    /// # Errors
    /// [`EngineError::Store`] if the store cannot read its head.
    pub fn update_basis(&self) -> Result<UpdateBasis, EngineError> {
        Ok(UpdateBasis {
            instance: self.instance().clone(),
            package: self.model().header().identity.clone(),
            head: self.head()?,
            clock: self.now(),
        })
    }

    /// Commit a [`PreparedUpdate`] (§20.4), adopting the target as the new active
    /// definition in one atomic commit.
    ///
    /// The plan's [`UpdateBasis`] is re-checked against the instance FIRST. A plan
    /// whose basis has moved — a commit landed, history moved, the clock advanced,
    /// or it belongs to another instance — is refused as
    /// [`UpdateError::Stale`] and nothing is committed: a plan is a statement about
    /// a state, and a state that has moved is not the one it describes. Re-prepare
    /// against the current position instead.
    ///
    /// # Errors
    /// [`UpdateError::Stale`] when the plan's basis has moved; [`UpdateError::Engine`]
    /// when the commit itself faults.
    pub fn apply_update(&mut self, prepared: PreparedUpdate) -> Result<UpdateReport, UpdateError> {
        let current = self.update_basis().map_err(UpdateError::Engine)?;
        if prepared.basis != current {
            return Err(UpdateError::Stale {
                prepared: Box::new(prepared.basis),
                current: Box::new(current),
            });
        }
        let PreparedUpdate { compilation, definition, relation, reconciliation, migrated, seeded, .. } = prepared;
        let commit = self
            .apply_migration(&definition, compilation, reconciliation.merged)
            .map_err(UpdateError::Engine)?;
        Ok(UpdateReport { relation, commit, migrated, seeded })
    }
}
