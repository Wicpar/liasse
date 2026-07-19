//! Keyring dynamic semantics (§17): the logical version lifecycle, rotation
//! scheduling on the engine's virtual clock, and acceptance policy, over a
//! host-supplied [`KeyProvider`](liasse_host::KeyProvider).
//!
//! The application declares policy (`$algorithm`/`$usage`/`$rotate`/`$retain`/
//! `$protection`); the provider maps it to physical keystores. This module owns
//! the observable state the spec pins:
//!
//! - **Version lifecycle** (§17.3): `pending -> active -> retired -> destroyed`,
//!   with `active -> revoked` immediate rejection. At most one signing version
//!   is active at one state position.
//! - **Rotation on the virtual clock** (§17.4): a due rotation is performed
//!   before the next operation ([`Keyring::ensure_current`]); `$overlap` exposes
//!   the next version as `pending` ahead of the cutover; the resulting logical
//!   order and key selection are independent of when the runtime schedules it.
//! - **Sealed values** (§17.2): a [`KeyVersion`] carries only public metadata;
//!   the private [`KeyHandle`] never leaves this boundary and no public accessor
//!   surfaces private bytes, so private material is never an application value.
//! - **Failure keep-current** (§17.9): a provider failure during rotation leaves
//!   the current version active and marks the ring overdue; an unavailable
//!   signing operation rejects the requesting mutation, committing no effect.
//!
//! Provider capabilities are checked against the declared policy at construction
//! (§17.6). The COSE-style token encoding a package signs with (§17.7/§17.8) is a
//! host namespace guarded by [`ConformanceGuard`](liasse_host::ConformanceGuard);
//! this module exposes the restricted active version and the acceptance set that
//! such verification consults.

use std::collections::BTreeSet;

use liasse_host::{
    ExternalKeyRef, KeyCapabilities, KeyHandle, KeyOperation, KeyProvider, KeySpec, ProtectionClass,
    ProviderRequirement, PublicKeyError,
};
use liasse_value::{Duration, Precision, Timestamp, Value};

/// A logical key-version identity, monotonic within one keyring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VersionId(u64);

impl VersionId {
    /// The version's ordinal.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The lifecycle state of a key version (§17.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    /// Provider handle bound and public metadata verified, not yet active.
    Pending,
    /// Selected for new operations.
    Active,
    /// Accepted for verification per `$retain`.
    Retired,
    /// Rejected immediately (§17.3).
    Revoked,
    /// Provider material destroyed; only audit metadata remains.
    Destroyed,
}

/// The application-visible metadata of one key version (§17.2). Private key
/// bytes and provider credentials are never carried here; the opaque provider
/// [`KeyHandle`] is kept out of the public surface entirely.
#[derive(Debug, Clone)]
pub struct KeyVersion {
    id: VersionId,
    algorithm: String,
    public_key: Value,
    created_at: Timestamp,
    activated_at: Option<Timestamp>,
    retired_at: Option<Timestamp>,
    revoked_at: Option<Timestamp>,
    attestation: Option<Value>,
    state: KeyState,
    handle: KeyHandle,
}

impl KeyVersion {
    /// The logical version id.
    #[must_use]
    pub const fn id(&self) -> VersionId {
        self.id
    }

    /// The key algorithm.
    #[must_use]
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }

    /// The application-safe public key material (§17.2).
    #[must_use]
    pub fn public_key(&self) -> &Value {
        &self.public_key
    }

    /// When the version was created.
    #[must_use]
    pub const fn created_at(&self) -> Timestamp {
        self.created_at
    }

    /// When the version was activated, if it has been.
    #[must_use]
    pub const fn activated_at(&self) -> Option<Timestamp> {
        self.activated_at
    }

    /// When the version was retired, if it has been.
    #[must_use]
    pub const fn retired_at(&self) -> Option<Timestamp> {
        self.retired_at
    }

    /// When the version was revoked, if it has been.
    #[must_use]
    pub const fn revoked_at(&self) -> Option<Timestamp> {
        self.revoked_at
    }

    /// The provider attestation, if any.
    #[must_use]
    pub const fn attestation(&self) -> Option<&Value> {
        self.attestation.as_ref()
    }

    /// The current lifecycle state.
    #[must_use]
    pub const fn state(&self) -> KeyState {
        self.state
    }
}

/// Whether scheduled rotation is automatic or driven by a host operator (§17.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationMode {
    /// The runtime generates and activates new versions on cadence.
    Automatic,
    /// A host operator binds and activates each externally created version.
    Manual,
}

/// The `$rotate` schedule (§17.1): cadence, overlap lead time, and mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationSchedule {
    /// The `$every` cadence.
    pub every: Duration,
    /// The `$overlap` lead time before cutover (zero when omitted).
    pub overlap: Duration,
    /// Automatic or manual.
    pub mode: RotationMode,
}

/// The observable keyring policy (§17.1): what a provider must satisfy and how
/// versions rotate and are retained.
#[derive(Debug, Clone)]
pub struct KeyringPolicy {
    /// The `$algorithm`.
    pub algorithm: String,
    /// The `$usage` operation set (or the inferred minimal set).
    pub usage: BTreeSet<KeyOperation>,
    /// The `$rotate` schedule; `None` disables scheduled rotation.
    pub rotate: Option<RotationSchedule>,
    /// The `$retain` window for retired versions; `None` accepts until explicit
    /// revocation or destruction (§17.1).
    pub retain: Option<Duration>,
    /// The required `$protection` class, if declared.
    pub protection: Option<ProtectionClass>,
}

impl KeyringPolicy {
    /// The §17.6 capability demand this policy places on its provider.
    #[must_use]
    pub fn requirement(&self) -> ProviderRequirement {
        let automatic = matches!(self.rotate.map(|r| r.mode), Some(RotationMode::Automatic) | None);
        let external_binding = matches!(self.rotate.map(|r| r.mode), Some(RotationMode::Manual));
        ProviderRequirement {
            algorithm: self.algorithm.clone(),
            operations: self.usage.clone(),
            automatic,
            external_binding,
            protection: self.protection,
            needs_disable: self.rotate.is_some(),
            needs_destroy: false,
            needs_attestation: false,
        }
    }

    fn spec(&self) -> KeySpec {
        KeySpec {
            algorithm: self.algorithm.clone(),
            operations: self.usage.clone(),
            protection: self.protection,
        }
    }
}

/// Why a keyring operation could not proceed (§17.6, §17.9).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyringError {
    /// The provider does not satisfy the declared policy (§17.6).
    #[error("provider fails keyring capability check: {0}")]
    Capability(#[from] liasse_host::CapabilityShortfall),
    /// The provider could not perform a lifecycle operation (§17.9).
    #[error("provider operation failed: {0}")]
    Provider(#[from] liasse_host::ProviderFailure),
    /// The provider returned a structurally invalid public key (§17.4 step 2).
    #[error("provider returned an invalid public key: {0}")]
    PublicKey(#[from] PublicKeyError),
    /// A bound or generated version's public metadata names a different
    /// algorithm than the policy declares (§17.4 validate-public-metadata).
    #[error("version algorithm `{found}` does not match policy `{declared}`")]
    AlgorithmMismatch {
        /// The policy's declared algorithm.
        declared: String,
        /// The version's advertised algorithm.
        found: String,
    },
    /// No active version exists to serve the operation (§17.3).
    #[error("keyring has no active version")]
    NoActiveVersion,
    /// The addressed version is not present in this ring.
    #[error("keyring has no version {0}")]
    UnknownVersion(u64),
}

/// The outcome of a `ensure_current`/rotation attempt (§17.4, §17.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationOutcome {
    /// No rotation was due.
    NotDue,
    /// A due rotation completed: the prior version retired and a new one is
    /// active.
    Rotated(VersionId),
    /// A rotation was due but the provider failed; the current version stays
    /// active and the ring is overdue (§17.9).
    KeptCurrentOverdue,
}

/// A signing token bound to the ring identity and the version that signed it
/// (§17.7/§17.8). The verifier consults the ring's acceptance set, so a token
/// from a revoked or foreign version fails verification even though its bytes
/// are unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionToken {
    ring: String,
    version: VersionId,
    signature: Vec<u8>,
}

impl SessionToken {
    /// The version identity the accepting namespace reads (§17.7).
    #[must_use]
    pub const fn version(&self) -> VersionId {
        self.version
    }

    /// The signing ring name.
    #[must_use]
    pub fn ring(&self) -> &str {
        &self.ring
    }

    /// The provider's genuine signature over the signed payload (§17.7/§17.8):
    /// the bytes a cose token carries as `$sig` and a verifier checks against the
    /// accepted version's public key. This is the real signing output — never the
    /// plaintext claim bytes — so a token cannot be minted from public metadata.
    #[must_use]
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }
}

/// Why a token failed verification (§17.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The token was signed by a different keyring.
    #[error("token was signed by a different keyring")]
    WrongRing,
    /// The token's version is not currently accepted (retired past `$retain`,
    /// revoked, or destroyed).
    #[error("token version is no longer accepted")]
    VersionNotAccepted,
}

/// A managed keyring over a host key provider `P` (§17).
pub struct Keyring<P> {
    name: String,
    provider: P,
    policy: KeyringPolicy,
    versions: Vec<KeyVersion>,
    next_id: u64,
    overdue: bool,
}

impl<P: KeyProvider> Keyring<P> {
    /// Load a keyring named `name` over `provider`, checking provider
    /// capabilities against `policy` at load (§17.6). Returns a
    /// [`KeyringError::Capability`] when the provider is incompatible.
    pub fn load(
        name: impl Into<String>,
        provider: P,
        policy: KeyringPolicy,
    ) -> Result<Self, KeyringError> {
        let capabilities: KeyCapabilities = provider.capabilities();
        capabilities.satisfies(&policy.requirement())?;
        Ok(Self {
            name: name.into(),
            provider,
            policy,
            versions: Vec::new(),
            next_id: 1,
            overdue: false,
        })
    }

    /// Bootstrap the first version (§17.3). Automatic mode generates and
    /// activates it; manual mode leaves the ring without an active version until
    /// an operator [`Keyring::bind_activate`]s one, so a dependent surface stays
    /// unavailable.
    pub fn bootstrap(&mut self, now: Timestamp) -> Result<(), KeyringError> {
        let manual = matches!(self.policy.rotate.map(|r| r.mode), Some(RotationMode::Manual));
        if manual {
            return Ok(());
        }
        let handle = self.provider.generate(&self.policy.spec())?;
        let version = self.record(handle, now)?;
        self.activate(version, now);
        Ok(())
    }

    /// The ring name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Mutable access to the backing provider, for host-driven reconfiguration
    /// (the `provider_set` fault-injection vocabulary of §17.9).
    pub fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }

    /// Whether a scheduled rotation is overdue after a keep-current failure
    /// (§17.9). Reported through health and diagnostics.
    #[must_use]
    pub const fn is_overdue(&self) -> bool {
        self.overdue
    }

    /// The active version, if the ring has one (§17.3).
    #[must_use]
    pub fn current(&self) -> Option<&KeyVersion> {
        self.versions.iter().find(|v| v.state == KeyState::Active)
    }

    /// Every retained version's metadata (§17.2 `ring.$versions`).
    #[must_use]
    pub fn versions(&self) -> &[KeyVersion] {
        &self.versions
    }

    /// The versions accepted for verification at `now` (§17.2 `ring.$accepted`):
    /// the active version and every retired version still inside its `$retain`
    /// window, excluding revoked and destroyed versions.
    #[must_use]
    pub fn accepted(&self, now: Timestamp) -> Vec<&KeyVersion> {
        self.versions.iter().filter(|v| self.is_accepted(v, now)).collect()
    }

    fn is_accepted(&self, version: &KeyVersion, now: Timestamp) -> bool {
        match version.state {
            KeyState::Active => true,
            KeyState::Retired => match (self.policy.retain, version.retired_at) {
                // §17.1: an omitted `$retain` accepts until revoked/destroyed.
                (None, _) => true,
                (Some(retain), Some(retired_at)) => now < instant_add(retired_at, retain),
                (Some(_), None) => true,
            },
            KeyState::Pending | KeyState::Revoked | KeyState::Destroyed => false,
        }
    }

    /// Perform any due rotation before the next operation (§17.4). One idle gap
    /// that spans several cadences is caught up in full: one rotation is
    /// performed for EVERY whole elapsed cadence, each atomic cutover placed at
    /// its scheduled boundary instant, so the lazy result's logical order and
    /// key selection are identical to a runtime that rotated on schedule, and the
    /// next boundary continues from the last boundary rather than the late
    /// operation time (no cadence drift). Idempotent for a given clock: a second
    /// call at the same instant finds nothing due. A provider failure keeps the
    /// current version active and stops the catch-up (§17.9).
    pub fn ensure_current(&mut self, now: Timestamp) -> RotationOutcome {
        let Some(schedule) = self.policy.rotate else { return RotationOutcome::NotDue };
        if schedule.mode != RotationMode::Automatic {
            return RotationOutcome::NotDue;
        }
        let Some(active) = self.current() else { return RotationOutcome::NotDue };
        let Some(activated_at) = active.activated_at else { return RotationOutcome::NotDue };
        let mut due_at = instant_add(activated_at, schedule.every);
        if now < due_at {
            self.expose_pending(now, schedule, due_at);
            return RotationOutcome::NotDue;
        }
        // At least one whole cadence has elapsed. §17.4 fixes the lazy result to
        // the scheduled one: rotate once per elapsed cadence, each cutover at its
        // scheduled boundary (`due_at`) — NOT the late operation time `now`.
        // Activating at the boundary keeps the logical order, key selection, and
        // the prior version's retirement/`$retain` instants identical to a
        // scheduled runtime, and leaves the next boundary at `boundary + $every`
        // so no missed cadence is silently absorbed (§17.3/§17.4).
        let mut outcome = RotationOutcome::NotDue;
        while now >= due_at {
            match self.rotate(due_at) {
                Ok(id) => {
                    self.overdue = false;
                    outcome = RotationOutcome::Rotated(id);
                }
                Err(_) => {
                    // §17.9: keep the current version active, report overdue, and
                    // stop catching up.
                    self.overdue = true;
                    return RotationOutcome::KeptCurrentOverdue;
                }
            }
            let next_due = instant_add(due_at, schedule.every);
            if next_due <= due_at {
                // A non-advancing cadence (a zero/negative `$every`, one below a
                // single clock tick, or a saturated clock) defines no further
                // boundary. Bound the catch-up to this one rotation so a
                // degenerate policy cannot spin instead of looping forever.
                break;
            }
            due_at = next_due;
        }
        outcome
    }

    /// Expose the next version as `pending` once the `$overlap` lead is reached
    /// (§17.4 step 3), ahead of the atomic cutover.
    fn expose_pending(&mut self, now: Timestamp, schedule: RotationSchedule, due_at: Timestamp) {
        if self.versions.iter().any(|v| v.state == KeyState::Pending) {
            return;
        }
        let lead_start = sub_duration(due_at, schedule.overlap);
        if now < lead_start {
            return;
        }
        if let Ok(handle) = self.provider.generate(&self.policy.spec()) {
            let _ = self.record(handle, now);
        }
    }

    /// The full automatic-rotation transition (§17.4): generate, read+validate,
    /// activate, retire the prior active version, disable it at the provider.
    fn rotate(&mut self, now: Timestamp) -> Result<VersionId, KeyringError> {
        let pending = self.versions.iter().find(|v| v.state == KeyState::Pending).map(|v| v.id);
        let new_id = match pending {
            Some(id) => id,
            None => {
                let handle = self.provider.generate(&self.policy.spec())?;
                self.record(handle, now)?
            }
        };
        self.activate(new_id, now);
        Ok(new_id)
    }

    /// Bind an externally created handle and activate it through the same
    /// transition as automatic rotation (§17.4 manual policy).
    pub fn bind_activate(
        &mut self,
        external: &ExternalKeyRef,
        now: Timestamp,
    ) -> Result<VersionId, KeyringError> {
        let handle = self.provider.bind(external, &self.policy.spec())?;
        let id = self.record(handle, now)?;
        self.activate(id, now);
        Ok(id)
    }

    /// Revoke a version (§17.3): it is rejected immediately for verification,
    /// overriding any remaining `$retain` window.
    pub fn revoke(&mut self, version: VersionId, now: Timestamp) -> Result<(), KeyringError> {
        let target = self.version_mut(version)?;
        target.state = KeyState::Revoked;
        target.revoked_at = Some(now);
        Ok(())
    }

    /// Destroy a version's provider material (§17.3): it is no longer accepted
    /// and only audit metadata remains.
    pub fn destroy(&mut self, version: VersionId, _now: Timestamp) -> Result<(), KeyringError> {
        let handle = self.version_ref(version)?.handle;
        self.provider.destroy(&handle)?;
        let target = self.version_mut(version)?;
        target.state = KeyState::Destroyed;
        Ok(())
    }

    /// Sign `message` with the active version, returning a ring-and-version-bound
    /// token (§17.7/§17.8). An unavailable signing operation rejects the request
    /// and commits no effect (§17.9).
    pub fn sign(&self, message: &[u8], _now: Timestamp) -> Result<SessionToken, KeyringError> {
        let active = self.current().ok_or(KeyringError::NoActiveVersion)?;
        let signature = self.provider.sign(&active.handle, &active.algorithm, message)?;
        Ok(SessionToken { ring: self.name.clone(), version: active.id, signature })
    }

    /// Verify a token against the acceptance set at `now` (§17.7): the token's
    /// ring must match and its version must be currently accepted.
    pub fn verify(&self, token: &SessionToken, now: Timestamp) -> Result<VersionId, VerifyError> {
        if token.ring != self.name {
            return Err(VerifyError::WrongRing);
        }
        if self.accepted(now).iter().any(|v| v.id == token.version) {
            Ok(token.version)
        } else {
            Err(VerifyError::VersionNotAccepted)
        }
    }

    /// Record a pending version from a provider handle, reading and validating
    /// its public key (§17.4 step 2). An invalid public key is rejected here so
    /// §17.9 keeps the current version.
    fn record(&mut self, handle: KeyHandle, now: Timestamp) -> Result<VersionId, KeyringError> {
        let public = self.provider.public_key(&handle)?;
        public.validate()?;
        // §17.4: validate the public metadata against the declared policy — a
        // bound key advertising a different algorithm is rejected here, so §17.9
        // keeps the current version.
        if public.algorithm() != self.policy.algorithm {
            return Err(KeyringError::AlgorithmMismatch {
                declared: self.policy.algorithm.clone(),
                found: public.algorithm().to_owned(),
            });
        }
        let attestation = self.provider.attest(&handle)?.map(|a| a.material().clone());
        let id = VersionId(self.next_id);
        self.next_id += 1;
        self.versions.push(KeyVersion {
            id,
            algorithm: public.algorithm().to_owned(),
            public_key: public.material().clone(),
            created_at: now,
            activated_at: None,
            retired_at: None,
            revoked_at: None,
            attestation,
            state: KeyState::Pending,
            handle,
        });
        Ok(id)
    }

    /// Atomically activate `version`, retiring the prior active version (§17.4
    /// step 4). At most one signing version is active afterward.
    fn activate(&mut self, version: VersionId, now: Timestamp) {
        for existing in &mut self.versions {
            if existing.state == KeyState::Active {
                existing.state = KeyState::Retired;
                existing.retired_at = Some(now);
            }
        }
        if let Some(target) = self.versions.iter_mut().find(|v| v.id == version) {
            target.state = KeyState::Active;
            target.activated_at = Some(now);
        }
    }

    fn version_ref(&self, version: VersionId) -> Result<&KeyVersion, KeyringError> {
        self.versions
            .iter()
            .find(|v| v.id == version)
            .ok_or(KeyringError::UnknownVersion(version.0))
    }

    fn version_mut(&mut self, version: VersionId) -> Result<&mut KeyVersion, KeyringError> {
        self.versions
            .iter_mut()
            .find(|v| v.id == version)
            .ok_or(KeyringError::UnknownVersion(version.0))
    }
}

/// Add an elapsed `duration` to a `timestamp`, preserving the timestamp's
/// precision (§14 clock arithmetic). Saturates rather than overflowing.
fn instant_add(timestamp: Timestamp, duration: Duration) -> Timestamp {
    shift(timestamp, ticks_of(duration, timestamp.precision()))
}

/// Subtract an elapsed `duration` from a `timestamp`.
fn sub_duration(timestamp: Timestamp, duration: Duration) -> Timestamp {
    shift(timestamp, ticks_of(duration, timestamp.precision()).saturating_neg())
}

fn shift(timestamp: Timestamp, ticks: i128) -> Timestamp {
    Timestamp::new(timestamp.count().saturating_add(ticks), timestamp.precision())
}

/// The number of `precision` ticks in an elapsed nanosecond duration.
fn ticks_of(duration: Duration, precision: Precision) -> i128 {
    const NS_PER_SEC: i128 = 1_000_000_000;
    duration.as_nanos().saturating_mul(precision.ticks_per_second()) / NS_PER_SEC
}
