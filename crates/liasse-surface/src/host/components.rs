//! Composing §17 keyrings and §18 blob hosts into the surface call path
//! (SPEC.md §17.7/§17.8, §18.7).
//!
//! The standalone [`CoseKeyring`] and [`BlobHost`] façades own the host
//! machinery; this module hangs them off the [`SurfaceHost`] under the names a
//! call site addresses (`cose.sign(/session_keys, …)`, a blob-typed parameter),
//! and exposes the driver-facing vocabulary a scenario adapter provisions a
//! case's `hosts` block against and drives:
//!
//! - keyring: [`register_keyring`](SurfaceHost::register_keyring),
//!   [`keyring_bootstrap`](SurfaceHost::keyring_bootstrap),
//!   [`keyring_sign`](SurfaceHost::keyring_sign),
//!   [`keyring_verify`](SurfaceHost::keyring_verify),
//!   [`keyring_rotate`](SurfaceHost::keyring_rotate),
//!   [`keyring_bind_activate`](SurfaceHost::keyring_bind_activate),
//!   [`keyring_revoke`](SurfaceHost::keyring_revoke),
//!   [`keyring_destroy`](SurfaceHost::keyring_destroy),
//!   [`keyring_advance`](SurfaceHost::keyring_advance), and
//!   [`provider_mut`](SurfaceHost::provider_mut) for the §17.9 `provider_set`
//!   fault-injection vocabulary;
//! - blob: [`register_blob`](SurfaceHost::register_blob),
//!   [`blob_put`](SurfaceHost::blob_put), [`blob_get`](SurfaceHost::blob_get),
//!   [`blob_reconcile`](SurfaceHost::blob_reconcile), and
//!   [`connector_mut`](SurfaceHost::connector_mut) for the §18.12 `connector_set`
//!   vocabulary;
//! - admission: [`call_with_blob`](SurfaceHost::call_with_blob) stages and
//!   verifies a blob parameter (§18.2/§18.7) and binds the verified descriptor
//!   into the surface call before it is admitted.

use liasse_host::sim::{SimConnector, SimKeyProvider};
use liasse_host::{CoseClaims, CoseToken};
use liasse_runtime::{
    DeclaredDescriptor, KeyringError, Rejection, RejectionReason, RotationOutcome, StoreId,
    Timestamp, VersionId,
};
use liasse_store::InstanceStore;

use crate::blobs::{BlobGetOutcome, BlobHost, BlobPutOutcome};
use crate::cose::{CoseKeyring, CoseVerifyError};
use crate::keyring::KeyringAdmin;
use crate::outcome::SurfaceOutcome;
use crate::request::SurfaceCall;

use super::{SurfaceError, SurfaceHost};

/// A driver op named a host component that was never registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostComponentError {
    /// No keyring is composed under this name.
    #[error("no keyring named `{0}` is composed into this host")]
    NoKeyring(String),
    /// No blob host is composed under this name.
    #[error("no blob host named `{0}` is composed into this host")]
    NoBlob(String),
}

impl<S: InstanceStore> SurfaceHost<S> {
    // ---- keyring composition (§17) ---------------------------------------

    /// Compose a §17 keyring under `name` (the `$keyring` declaration name a
    /// `cose.sign`/`cose.verify` call addresses as `/name`).
    pub fn register_keyring(&mut self, name: impl Into<String>, ring: CoseKeyring<SimKeyProvider>) {
        self.keyrings.insert(name.into(), ring);
    }

    /// Whether a keyring is composed under `name`.
    #[must_use]
    pub fn has_keyring(&self, name: &str) -> bool {
        self.keyrings.contains_key(name)
    }

    fn keyring(&self, name: &str) -> Result<&CoseKeyring<SimKeyProvider>, HostComponentError> {
        self.keyrings.get(name).ok_or_else(|| HostComponentError::NoKeyring(name.to_owned()))
    }

    fn keyring_mut(&mut self, name: &str) -> Result<&mut CoseKeyring<SimKeyProvider>, HostComponentError> {
        self.keyrings.get_mut(name).ok_or_else(|| HostComponentError::NoKeyring(name.to_owned()))
    }

    /// Bootstrap the composed keyring `ring`'s first version (§17.3).
    ///
    /// # Errors
    /// [`HostComponentError::NoKeyring`] if unregistered; [`KeyringError`] on a
    /// provider or validation failure.
    pub fn keyring_bootstrap(&mut self, ring: &str) -> Result<(), KeyringErrorOr> {
        self.keyring_mut(ring)?.admin_mut().bootstrap().map_err(KeyringErrorOr::Keyring)
    }

    /// Resolve keyring `ring` and sign `claims` into a token (§17.7 `cose.sign`).
    ///
    /// # Errors
    /// [`HostComponentError::NoKeyring`] if unregistered; [`KeyringError`] when
    /// the provider cannot sign (§17.9) — no token is produced.
    pub fn keyring_sign(&self, ring: &str, claims: CoseClaims) -> Result<CoseToken, KeyringErrorOr> {
        self.keyring(ring)?.sign(claims).map_err(KeyringErrorOr::Keyring)
    }

    /// Resolve keyring `ring` and verify `token` against its accepted versions at
    /// the ring's current instant (§17.7 `cose.verify`), returning the verified
    /// claims and version identity.
    ///
    /// # Errors
    /// [`HostComponentError::NoKeyring`] if unregistered; [`CoseVerifyError`] for
    /// a foreign-ring, no-longer-accepted, or tampered token.
    pub fn keyring_verify(
        &self,
        ring: &str,
        token: &CoseToken,
    ) -> Result<(CoseClaims, VersionId), VerifyErrorOr> {
        self.keyring(ring)?.verify(token).map_err(VerifyErrorOr::Verify)
    }

    /// Perform any due scheduled rotation on `ring` at its current instant
    /// (§17.4).
    ///
    /// # Errors
    /// [`HostComponentError::NoKeyring`] if unregistered.
    pub fn keyring_rotate(&mut self, ring: &str) -> Result<RotationOutcome, HostComponentError> {
        Ok(self.keyring_mut(ring)?.admin_mut().ensure_current())
    }

    /// Bind an externally created handle and activate it on `ring` (§17.4 manual
    /// policy).
    ///
    /// # Errors
    /// [`HostComponentError::NoKeyring`] if unregistered; [`KeyringError`] on a
    /// bind/validation failure.
    pub fn keyring_bind_activate(
        &mut self,
        ring: &str,
        external: &liasse_host::ExternalKeyRef,
    ) -> Result<VersionId, KeyringErrorOr> {
        self.keyring_mut(ring)?.admin_mut().bind_activate(external).map_err(KeyringErrorOr::Keyring)
    }

    /// Revoke a version on `ring` (§17.3): rejected immediately for verification.
    ///
    /// # Errors
    /// [`HostComponentError::NoKeyring`] if unregistered; [`KeyringError`] for an
    /// unknown version.
    pub fn keyring_revoke(&mut self, ring: &str, version: VersionId) -> Result<(), KeyringErrorOr> {
        self.keyring_mut(ring)?.admin_mut().revoke(version).map_err(KeyringErrorOr::Keyring)
    }

    /// Destroy a version's provider material on `ring` (§17.3).
    ///
    /// # Errors
    /// [`HostComponentError::NoKeyring`] if unregistered; [`KeyringError`] on
    /// failure.
    pub fn keyring_destroy(&mut self, ring: &str, version: VersionId) -> Result<(), KeyringErrorOr> {
        self.keyring_mut(ring)?.admin_mut().destroy(version).map_err(KeyringErrorOr::Keyring)
    }

    /// Advance keyring `ring`'s instant to `now` (§17.3/§17.4): crosses a
    /// `$retain` boundary or a rotation cadence, so acceptance is re-evaluated.
    ///
    /// # Errors
    /// [`HostComponentError::NoKeyring`] if unregistered.
    pub fn keyring_advance(&mut self, ring: &str, now: Timestamp) -> Result<(), HostComponentError> {
        self.keyring_mut(ring)?.admin_mut().clock_mut().set(now.count());
        Ok(())
    }

    /// Mutable access to keyring `ring`'s backing provider, for the §17.9
    /// `provider_set` fault-injection vocabulary (unavailability, per-operation
    /// failure, an invalid public key).
    ///
    /// # Errors
    /// [`HostComponentError::NoKeyring`] if unregistered.
    pub fn provider_mut(&mut self, ring: &str) -> Result<&mut SimKeyProvider, HostComponentError> {
        Ok(self.keyring_mut(ring)?.admin_mut().provider_mut())
    }

    /// The composed keyring's managed ring, for reading version metadata (§17.2).
    ///
    /// # Errors
    /// [`HostComponentError::NoKeyring`] if unregistered.
    pub fn keyring_admin(&self, ring: &str) -> Result<&KeyringAdmin<SimKeyProvider>, HostComponentError> {
        Ok(self.keyring(ring)?.admin())
    }

    // ---- blob composition (§18) ------------------------------------------

    /// Compose a §18 blob host under `name` (the accepted blob-field name a
    /// blob-typed parameter binds to).
    pub fn register_blob(&mut self, name: impl Into<String>, blob: BlobHost<SimConnector>) {
        self.blobs.insert(name.into(), blob);
    }

    /// Whether a blob host is composed under `name`.
    #[must_use]
    pub fn has_blob(&self, name: &str) -> bool {
        self.blobs.contains_key(name)
    }

    fn blob(&self, name: &str) -> Result<&BlobHost<SimConnector>, HostComponentError> {
        self.blobs.get(name).ok_or_else(|| HostComponentError::NoBlob(name.to_owned()))
    }

    fn blob_mut(&mut self, name: &str) -> Result<&mut BlobHost<SimConnector>, HostComponentError> {
        self.blobs.get_mut(name).ok_or_else(|| HostComponentError::NoBlob(name.to_owned()))
    }

    /// Put `bytes` as `media` into blob host `name` (§18.7), returning the
    /// committed digest/placement or the typed rejection.
    ///
    /// # Errors
    /// [`HostComponentError::NoBlob`] if unregistered.
    pub fn blob_put(
        &mut self,
        name: &str,
        bytes: &[u8],
        media: &str,
    ) -> Result<BlobPutOutcome, HostComponentError> {
        Ok(self.blob_mut(name)?.put(bytes, media))
    }

    /// Put `bytes` under a client-`declared` descriptor into blob host `name`
    /// (§18.7): a lying descriptor is rejected before any copy lands (§18.1).
    ///
    /// # Errors
    /// [`HostComponentError::NoBlob`] if unregistered.
    pub fn blob_put_declared(
        &mut self,
        name: &str,
        declared: &DeclaredDescriptor,
        bytes: &[u8],
    ) -> Result<BlobPutOutcome, HostComponentError> {
        Ok(self.blob_mut(name)?.put_declared(declared, bytes))
    }

    /// Fetch content `digest` from blob host `name` through a §18.8 visibility
    /// gate.
    ///
    /// # Errors
    /// [`HostComponentError::NoBlob`] if unregistered.
    pub fn blob_get(
        &self,
        name: &str,
        digest: &str,
        visible: bool,
    ) -> Result<BlobGetOutcome, HostComponentError> {
        Ok(self.blob(name)?.get(digest, visible))
    }

    /// Converge blob host `name`'s retained content `digest` toward policy
    /// (§18.6).
    ///
    /// # Errors
    /// [`HostComponentError::NoBlob`] if unregistered.
    pub fn blob_reconcile(&mut self, name: &str, digest: &str) -> Result<bool, HostComponentError> {
        Ok(self.blob_mut(name)?.reconcile(digest))
    }

    /// The verified stores holding `digest` in blob host `name` (`blob.$stored`,
    /// §18.5).
    ///
    /// # Errors
    /// [`HostComponentError::NoBlob`] if unregistered.
    pub fn blob_stored(&self, name: &str, digest: &str) -> Result<Option<Vec<StoreId>>, HostComponentError> {
        Ok(self.blob(name)?.stored(digest))
    }

    /// Mutable access to blob host `name`'s connector `connector`, for the §18.12
    /// `connector_set` fault-injection vocabulary (unavailability, corruption, a
    /// tampering read transport).
    ///
    /// # Errors
    /// [`HostComponentError::NoBlob`] if unregistered.
    pub fn connector_mut(
        &mut self,
        name: &str,
        connector: &str,
    ) -> Result<Option<&mut SimConnector>, HostComponentError> {
        Ok(self.blob_mut(name)?.connector_mut(connector))
    }

    // ---- blob-parameter admission (§18.7) --------------------------------

    /// Admit surface `call` on connection `id` with a §18.7 blob parameter: stage
    /// and verify `bytes`/`media` against blob host `blob_field`'s accepted type
    /// and placement policy, then bind the verified descriptor as the call's
    /// `blob_field` argument before admission.
    ///
    /// A blob whose descriptor does not verify (oversize, unaccepted media, hash
    /// mismatch) rejects the containing call before any state transition (§18.2),
    /// returned as a [`SurfaceOutcome::Rejected`] — no partial effect, no
    /// admission.
    ///
    /// # Errors
    /// [`SurfaceError::NoConnection`] if `id` is not open; a store fault from
    /// admission. [`HostComponentError::NoBlob`] surfaces as a
    /// [`SurfaceError::Engine`] internal fault only if `blob_field` is
    /// unregistered — a driver provisions it from the case `hosts` block first.
    pub fn call_with_blob(
        &mut self,
        id: &str,
        call: SurfaceCall,
        blob_field: &str,
        bytes: &[u8],
        media: &str,
    ) -> Result<SurfaceOutcome, SurfaceError> {
        let outcome = match self.blob_put(blob_field, bytes, media) {
            Ok(outcome) => outcome,
            Err(error) => {
                return Ok(SurfaceOutcome::Rejected(Rejection::new(
                    RejectionReason::Malformed,
                    error.to_string(),
                )))
            }
        };
        let digest = match outcome {
            BlobPutOutcome::Committed { digest, .. } => digest,
            BlobPutOutcome::Rejected(error) => {
                // §18.2: a failed verification rejects the containing call before
                // its state transition is admitted.
                return Ok(SurfaceOutcome::Rejected(Rejection::new(
                    RejectionReason::Malformed,
                    format!("blob parameter rejected: {error}"),
                )));
            }
        };
        // §18.7 step 4: bind the verified descriptor to the mutation parameter.
        let descriptor = self
            .blob(blob_field)
            .ok()
            .and_then(|blob| blob.descriptor_value(&digest));
        let call = match descriptor {
            Some(value) => call.with_arg(blob_field.to_owned(), value),
            None => call,
        };
        self.call(id, &call)
    }
}

/// A keyring driver op's failure: an unregistered component, or a §17 ring error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyringErrorOr {
    /// The named component was never composed into this host.
    #[error(transparent)]
    Component(#[from] HostComponentError),
    /// The ring rejected the operation (§17.3/§17.9).
    #[error(transparent)]
    Keyring(KeyringError),
}

/// A `cose.verify` driver op's failure: an unregistered component, or a §17.7
/// verification failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyErrorOr {
    /// The named component was never composed into this host.
    #[error(transparent)]
    Component(#[from] HostComponentError),
    /// The token did not verify against the accepted set (§17.7).
    #[error(transparent)]
    Verify(CoseVerifyError),
}
