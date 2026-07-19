//! COSE signing and verification composed over a managed keyring (SPEC.md
//! §17.7/§17.8).
//!
//! [`CoseKeyring`] is the surface composition the §17.8 direct-token flow needs:
//! it bundles a [`KeyringAdmin`] — which owns the version lifecycle over a host
//! [`KeyProvider`](liasse_host::KeyProvider) — with the host cose codec
//! ([`CoseClaims`]/[`CoseToken`]), so a driver can
//!
//! - [`sign`](CoseKeyring::sign) a claim set into a [`CoseToken`]. Signing goes
//!   through the ring's active version, so a §17.9 provider outage rejects it and
//!   no token is minted — matching "an unavailable signing operation rejects the
//!   requesting mutation".
//! - [`verify`](CoseKeyring::verify) a token against the *accepted public
//!   versions* at the ring's current instant (§17.7). No provider operation is
//!   involved, so an existing token keeps authenticating through a provider
//!   outage, while a revoked/retired-past-`$retain`/foreign-ring token is denied.
//!   The result carries the verified version identity so authentication policy
//!   can reject a disallowed version.
//!
//! Verification uses the accepted public versions exactly as §17.7 specifies:
//! the token's signature is cryptographically checked (§17.8) over the claims'
//! canonical bytes against the accepted version's PUBLIC key material — so a
//! token verifies only when it carries a genuine signature by an accepted
//! version's private key, and a forgery assembled from public metadata is
//! rejected. Acceptance still gates first (a revoked / retired-past-`$retain` /
//! foreign-ring token is denied before any crypto), and a tampered claim set no
//! longer matches its signature.

use liasse_host::{verify_cose_signature, CoseClaims, CoseToken, KeyProvider, SignatureError};
use liasse_runtime::VersionId;

use crate::keyring::KeyringAdmin;

/// Why a [`CoseToken`] failed verification (§17.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CoseVerifyError {
    /// The token names a different keyring than this one (§17.7).
    #[error("token was signed by a different keyring")]
    WrongRing,
    /// The token's version is not currently accepted — retired past `$retain`,
    /// revoked, or destroyed (§17.7).
    #[error("token version is no longer accepted")]
    VersionNotAccepted,
    /// The token's signature does not verify over its claims under the accepted
    /// version's public key — a forged token, or a claim set altered after signing.
    #[error("token claims do not match the signed payload")]
    ClaimsTampered,
    /// The accepted version names an algorithm the cose codec cannot verify
    /// (§17.8): reject loudly rather than fall back to any weaker check.
    #[error("token signature algorithm is unsupported")]
    UnsupportedAlgorithm,
}

impl From<SignatureError> for CoseVerifyError {
    /// Map a §17.8 codec signature failure to a token rejection: an unsupported
    /// algorithm stays loud, everything else is a claims/signature mismatch.
    fn from(error: SignatureError) -> Self {
        match error {
            SignatureError::UnsupportedAlgorithm(_) => Self::UnsupportedAlgorithm,
            SignatureError::Invalid | SignatureError::MalformedPublicKey(_) => Self::ClaimsTampered,
        }
    }
}

/// A managed keyring exposing the §17.7/§17.8 cose sign/verify vocabulary over a
/// host [`KeyProvider`] `P`.
pub struct CoseKeyring<P> {
    admin: KeyringAdmin<P>,
}

impl<P: KeyProvider> CoseKeyring<P> {
    /// Wrap a managed [`KeyringAdmin`] as a cose signer/verifier.
    #[must_use]
    pub fn new(admin: KeyringAdmin<P>) -> Self {
        Self { admin }
    }

    /// The managed ring, for lifecycle and clock control (bootstrap, rotation,
    /// revoke, `provider_set` fault injection).
    pub fn admin_mut(&mut self) -> &mut KeyringAdmin<P> {
        &mut self.admin
    }

    /// The managed ring.
    #[must_use]
    pub fn admin(&self) -> &KeyringAdmin<P> {
        &self.admin
    }

    /// The ring name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.admin.name()
    }

    /// Sign `claims` into a token through the active version (§17.8).
    ///
    /// # Errors
    /// [`KeyringError`](liasse_runtime::KeyringError) when the ring has no active
    /// version or the provider cannot sign (§17.9): no token is produced and no
    /// effect is committed.
    pub fn sign(&self, claims: CoseClaims) -> Result<CoseToken, liasse_runtime::KeyringError> {
        let signed = claims.signing_bytes();
        // §17.9: the sign exercises the provider's active-version operation, so a
        // provider outage rejects here before any token is minted.
        let session = self.admin.sign(&signed)?;
        // §17.8: the token carries the provider's GENUINE signature over the
        // signed bytes — never the plaintext claims — so it verifies only under
        // the active version's public key and cannot be forged from metadata.
        Ok(CoseToken::new(
            self.admin.name(),
            session.version().get(),
            claims,
            session.signature().to_vec(),
        ))
    }

    /// Verify `token` against the accepted public versions at the ring's current
    /// instant (§17.7), returning the verified claims and the version identity.
    ///
    /// # Errors
    /// [`CoseVerifyError`] for a foreign-ring, no-longer-accepted, or tampered
    /// token.
    pub fn verify(&self, token: &CoseToken) -> Result<(CoseClaims, VersionId), CoseVerifyError> {
        if token.ring() != self.admin.name() {
            return Err(CoseVerifyError::WrongRing);
        }
        // §17.7: verification uses the accepted PUBLIC versions. Locate the token's
        // version among them; a revoked / retired-past-`$retain` / never-accepted
        // version has no public key to verify against.
        let version = self
            .admin
            .accepted()
            .into_iter()
            .find(|version| version.id().get() == token.version())
            .ok_or(CoseVerifyError::VersionNotAccepted)?;
        // §17.7/§17.8: cryptographically verify the signature over the claims'
        // canonical bytes under that version's public key — a signature-blind
        // check would admit a token forged from public metadata.
        verify_cose_signature(
            version.algorithm(),
            version.public_key(),
            &token.claims().signing_bytes(),
            token.signature(),
        )?;
        Ok((token.claims().clone(), version.id()))
    }
}
