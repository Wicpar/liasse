//! Package evolution as a driver-facing lifecycle update (SPEC.md §20, §9.2–§9.3).
//!
//! A [`SurfaceHost`] already owns the runtime [`Engine`], which carries the whole
//! §20 migration machinery: [`Engine::update`] loads a target definition over the
//! active instance and commits the migrated state atomically (§20.1, §20.3). This
//! module lifts that engine operation to the host so a driver evolves a *running*
//! host **in place** — without tearing it down and losing its live connections.
//!
//! An in-place update is the migration analogue of an
//! [`import`](SurfaceHost::import) movement or an
//! [`operator_call`](SurfaceHost::operator_call): a committed update is an outgoing
//! frontier (§9.3), so it drags every open subscription through the new head
//! exactly as a client commit would (§12.6, §22.6). That sweep is where §12.2
//! coherence is enforced *across a definition change*: a subscription whose surface
//! the migration removed or renamed away no longer resolves at the new frontier and
//! is **closed**, while a subscription over a surviving surface is recomputed and
//! patched coherently. A rejected update leaves the instance — and every live
//! subscription — untouched.
//!
//! The exposed [`SurfaceRouter`] is derived from the *migrated* model (its public
//! and role surfaces are what the target definition declares), which only exists
//! once the update has been admitted. The caller therefore supplies a
//! `rebuild_router` that binds the new router against the migrated [`Engine`]; the
//! host swaps it in before the sweep, so both resolution and the completion
//! barrier's authority re-check see the target's surfaces rather than the
//! superseded ones. On a rejected update the router is never rebuilt and the prior
//! one stays in force.
//!
//! [`Engine`]: liasse_runtime::Engine
//! [`Engine::update`]: liasse_runtime::Engine::update

use liasse_runtime::{Engine, Rejection, UpdateError, UpdateReport};
use liasse_store::InstanceStore;

use crate::router::SurfaceRouter;

use super::{SurfaceError, SurfaceHost};

/// The observable result of an in-place lifecycle update (§20.3, §9.4).
///
/// A successful update is `Committed` (a new frontier, subscriptions swept) or
/// `Unchanged` (accepted but no state change, so no frontier and no sweep). A
/// refusal is a spec observation, not a fault: `Rejected` for an admission-pipeline
/// or boundary-narrowing refusal (§20.3, Annex E), `Invalid` for a statically
/// invalid target definition (§9.4), and `Incompatible` for a target on a different
/// compatibility line (§19.8). A store fault while migrating or sweeping is a
/// [`SurfaceError`], never an outcome.
#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    /// The migration committed as the new active definition; every open
    /// subscription was swept at the migration frontier (§12.2).
    Committed(UpdateReport),
    /// The target was accepted but changed no committed state (§20): the frontier
    /// did not advance, so no subscription was swept.
    Unchanged(UpdateReport),
    /// The migration was refused by the admission pipeline or a narrowing boundary
    /// contract (§20.3, Annex E) — the instance is unchanged.
    Rejected(Rejection),
    /// The target definition is statically invalid (§9.4) — the instance is
    /// unchanged.
    Invalid(String),
    /// The target is on a different compatibility line, so no update relation
    /// exists (§19.8) — the instance is unchanged.
    Incompatible(String),
}

impl<S: InstanceStore> SurfaceHost<S> {
    /// Update this host's instance to a target definition **in place** (§20, §9.2),
    /// keeping every live connection and subscription. The migration is admitted
    /// through the host's owned engine ([`Engine::update`]); on success the exposed
    /// router is rebound against the migrated model with `rebuild_router` and, when
    /// the update advanced the frontier, every open subscription is swept through
    /// the new head — closing any whose surface the migration removed or renamed
    /// away, and patching the rest coherently (§12.2, §12.6, §9.3). A rejected,
    /// invalid, or incompatible target leaves the instance, its router, and its
    /// subscriptions untouched.
    ///
    /// `rebuild_router` binds the new [`SurfaceRouter`] against the migrated
    /// [`Engine`] — the surfaces the target declares only exist once the update is
    /// admitted, so the caller cannot supply the router up front.
    ///
    /// # Errors
    /// [`SurfaceError::Engine`] from a store or view fault while sweeping, or a
    /// fault the caller's `rebuild_router` surfaces. A rejected, invalid, or
    /// incompatible migration is an [`UpdateOutcome`], not an error.
    pub fn update<F>(
        &mut self,
        target: &str,
        rebuild_router: F,
    ) -> Result<UpdateOutcome, SurfaceError>
    where
        F: FnOnce(&Engine<S>) -> Result<SurfaceRouter, SurfaceError>,
    {
        let before = self.engine.head();
        let report = match self.engine.update(target, &mut self.clock) {
            Ok(report) => report,
            Err(UpdateError::Rejected(rejection)) => return Ok(UpdateOutcome::Rejected(rejection)),
            Err(UpdateError::Incompatible(message)) => {
                return Ok(UpdateOutcome::Incompatible(message))
            }
            Err(UpdateError::Engine(engine)) => return Ok(UpdateOutcome::Invalid(engine.to_string())),
        };
        // §20.3: the migrated definition is now active. Rebind the exposed router to
        // the migrated model so resolution — and the completion barrier's authority
        // re-check below — see the target's surfaces, not the superseded ones.
        self.router = rebuild_router(&self.engine)?;
        if self.engine.head() == before {
            // §20: an accepted no-op produced no commit, so there is no outgoing
            // frontier for §12.2 to act on.
            return Ok(UpdateOutcome::Unchanged(report));
        }
        // §9.3/§12.6: the definition update is an outgoing frontier — drag every open
        // subscription through the migration head so a removed or renamed-away
        // surface closes (§12.2) and every survivor advances coherently.
        self.sweep_all()?;
        Ok(UpdateOutcome::Committed(report))
    }
}
