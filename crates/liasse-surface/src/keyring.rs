//! Keyring administration as driver-facing host operations (SPEC.md §17).
//!
//! The runtime [`Keyring`] owns the version lifecycle, rotation scheduling, sealed
//! public-only metadata, and the §17.9 keep-current failure handling over a host
//! [`KeyProvider`](liasse_host::KeyProvider). Every lifecycle and verification
//! operation is judged against an instant `now`.
//!
//! [`KeyringAdmin`] bundles that ring with a single owned [`VirtualClock`], so a
//! driver runs the `keyring_admin` vocabulary — bootstrap, scheduled rotation,
//! operator bind-activate, revoke, destroy — and the §17.7 sign/verify flow
//! against one deterministic clock, and reconfigures the backing provider through
//! [`provider_mut`](KeyringAdmin::provider_mut) for the §17.9 `provider_set`
//! fault-injection vocabulary. Rotation and acceptance are evaluated at the
//! clock's current instant, so advancing the clock is how a driver crosses a
//! `$retain` boundary or a rotation cadence.

use liasse_host::{ExternalKeyRef, KeyProvider};
use liasse_runtime::{
    KeyVersion, Keyring, KeyringError, RotationOutcome, SessionToken, Timestamp, VerifyError,
    VersionId,
};

use crate::clock::VirtualClock;

/// A managed §17 keyring driven over a single owned virtual clock.
pub struct KeyringAdmin<P> {
    ring: Keyring<P>,
    clock: VirtualClock,
}

impl<P: KeyProvider> KeyringAdmin<P> {
    /// Wrap a loaded `ring` driven by `clock`.
    #[must_use]
    pub fn new(ring: Keyring<P>, clock: VirtualClock) -> Self {
        Self { ring, clock }
    }

    /// The virtual clock, for advancing time across a rotation cadence or a
    /// `$retain` boundary.
    pub fn clock_mut(&mut self) -> &mut VirtualClock {
        &mut self.clock
    }

    /// The current instant the ring is judged against.
    #[must_use]
    pub fn now(&self) -> Timestamp {
        self.clock.instant()
    }

    /// The ring name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.ring.name()
    }

    /// Mutable access to the backing provider, for the §17.9 `provider_set`
    /// fault-injection vocabulary (unavailability, per-operation failure, an
    /// invalid public key).
    pub fn provider_mut(&mut self) -> &mut P {
        self.ring.provider_mut()
    }

    /// Bootstrap the first version at the current instant (§17.3). Automatic mode
    /// generates and activates it; manual mode waits for an operator
    /// [`bind_activate`](Self::bind_activate).
    ///
    /// # Errors
    /// [`KeyringError`] if the provider could not generate or its public metadata
    /// failed validation.
    pub fn bootstrap(&mut self) -> Result<(), KeyringError> {
        let now = self.clock.instant();
        self.ring.bootstrap(now)
    }

    /// Perform a due scheduled rotation at the current instant (§17.4). A provider
    /// failure keeps the current version active and reports overdue (§17.9).
    pub fn ensure_current(&mut self) -> RotationOutcome {
        let now = self.clock.instant();
        self.ring.ensure_current(now)
    }

    /// Bind an externally created handle and activate it (§17.4 manual policy).
    ///
    /// # Errors
    /// [`KeyringError`] if the provider could not bind or validate the handle.
    pub fn bind_activate(&mut self, external: &ExternalKeyRef) -> Result<VersionId, KeyringError> {
        let now = self.clock.instant();
        self.ring.bind_activate(external, now)
    }

    /// Revoke a version at the current instant (§17.3): rejected immediately for
    /// verification, overriding any remaining `$retain` window.
    ///
    /// # Errors
    /// [`KeyringError::UnknownVersion`] if the ring holds no such version.
    pub fn revoke(&mut self, version: VersionId) -> Result<(), KeyringError> {
        let now = self.clock.instant();
        self.ring.revoke(version, now)
    }

    /// Destroy a version's provider material (§17.3): no longer accepted, only
    /// audit metadata remains.
    ///
    /// # Errors
    /// [`KeyringError`] if the ring holds no such version or the provider failed.
    pub fn destroy(&mut self, version: VersionId) -> Result<(), KeyringError> {
        let now = self.clock.instant();
        self.ring.destroy(version, now)
    }

    /// Sign `message` with the active version (§17.7/§17.8).
    ///
    /// # Errors
    /// [`KeyringError::NoActiveVersion`] if the ring has no active version, or a
    /// provider failure.
    pub fn sign(&self, message: &[u8]) -> Result<SessionToken, KeyringError> {
        let now = self.clock.instant();
        self.ring.sign(message, now)
    }

    /// Verify a token against the acceptance set at the current instant (§17.7).
    ///
    /// # Errors
    /// [`VerifyError`] if the token is from a different ring or a version no longer
    /// accepted.
    pub fn verify(&self, token: &SessionToken) -> Result<VersionId, VerifyError> {
        let now = self.clock.instant();
        self.ring.verify(token, now)
    }

    /// The active version, if the ring has one (§17.3).
    #[must_use]
    pub fn current(&self) -> Option<&KeyVersion> {
        self.ring.current()
    }

    /// The versions accepted for verification at the current instant (§17.2
    /// `ring.$accepted`).
    #[must_use]
    pub fn accepted(&self) -> Vec<&KeyVersion> {
        let now = self.clock.instant();
        self.ring.accepted(now)
    }

    /// Whether a scheduled rotation is overdue after a keep-current failure
    /// (§17.9).
    #[must_use]
    pub fn is_overdue(&self) -> bool {
        self.ring.is_overdue()
    }
}
