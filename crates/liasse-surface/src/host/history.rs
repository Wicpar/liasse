//! History, artifacts, and reconciliation as driver-facing host operations
//! (SPEC.md §19).
//!
//! A [`SurfaceHost`] already owns the runtime [`Engine`], which carries the whole
//! §19 machinery: it exports its committed boundary as a verified `.liasse`
//! artifact, classifies an incoming artifact against local retained history,
//! imports one under a movement policy, and computes the §19.9 three-way merge.
//! This module lifts those engine operations to the host so a driver can produce
//! artifact bytes from one step and feed them to a later `import`/`reconcile`
//! step — the byte stream is the whole interchange, exactly as §19.5 pins it.
//!
//! Artifact verification (§19.8) lives entirely in the artifact layer: a tampered
//! or corrupt byte stream is rejected as an [`ImportError`] *before* any movement
//! is classified or applied, so a lying artifact never mutates committed state.
//! That rejection is a spec observation the driver renders (a refused import), so
//! these operations surface [`ImportError`] directly rather than folding it into
//! the transport-fault [`SurfaceError`].
//!
//! [`Engine`]: liasse_runtime::Engine

use liasse_runtime::{ImportError, ImportRelation, ImportReport, MergeOutcome};
use liasse_store::InstanceStore;

use super::{SurfaceError, SurfaceHost};

impl<S: InstanceStore> SurfaceHost<S> {
    /// Export the current committed boundary as a verified `.liasse` artifact
    /// (§19.5, §19.7): the active definition, the selected state, and a minimal
    /// history index naming the selected `(lineage, point)`. The returned bytes are
    /// what a later `import`/`reconcile` on this or another host consumes.
    ///
    /// # Errors
    /// [`SurfaceError::Engine`] if the boundary could not be captured or the
    /// artifact could not be built.
    pub fn export(&self) -> Result<Vec<u8>, SurfaceError> {
        Ok(self.engine.export()?)
    }

    /// Classify an incoming `artifact` against local retained history (§19.8)
    /// without applying any movement. Verification runs first, so a tampered
    /// artifact is an [`ImportError::Artifact`] and nothing is classified.
    ///
    /// # Errors
    /// [`ImportError`] if the byte stream fails recursive `.liasse` verification or
    /// its verified sections cannot be rebuilt.
    pub fn classify(&self, artifact: &[u8]) -> Result<ImportRelation, ImportError> {
        self.engine.classify(artifact)
    }

    /// Import `artifact` under a movement `policy` (§19.8): classify it, and when
    /// the relation is a `policy`-permitted fast-forward or rollback, move live
    /// state to the incoming point. A permitted movement that changes state drags
    /// every open subscription through the resulting head, exactly as a commit
    /// would (§12.6, §22.6).
    ///
    /// # Errors
    /// [`ImportError`] if the byte stream fails verification, or a store fault
    /// while re-installing the imported state.
    pub fn import(
        &mut self,
        artifact: &[u8],
        policy: &[ImportRelation],
    ) -> Result<ImportReport, ImportError> {
        let report = self.engine.import(artifact, policy)?;
        if report.applied {
            self.sweep_all().map_err(|error| match error {
                SurfaceError::Engine(engine) => ImportError::Engine(engine),
                SurfaceError::NoConnection(name) => {
                    ImportError::Corrupt(format!("no connection `{name}` while sweeping"))
                }
            })?;
        }
        Ok(report)
    }

    /// Compute the §19.9 automatic three-way merge (`reconcile`): `base` is the
    /// shared history point and `incoming` is the other side, both verified
    /// `.liasse` artifacts, against this host's current committed state. The
    /// returned [`MergeOutcome`] carries the unambiguous combined rows and any
    /// conflicts — the reconciliation plan a host correction would resolve. The
    /// merge is computed, never activated, so committed state is untouched.
    ///
    /// # Errors
    /// [`ImportError`] if either artifact fails verification or a section cannot be
    /// rebuilt against this host's schema.
    pub fn reconcile(&self, base: &[u8], incoming: &[u8]) -> Result<MergeOutcome, ImportError> {
        self.engine.merge(base, incoming)
    }
}
