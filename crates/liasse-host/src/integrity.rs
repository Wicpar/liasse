//! Blob read integrity: the §18.9 `fetch` verification step, which a runtime
//! must apply before delivering a successful result so a compromised connector
//! cannot make tampered bytes a successful fetch (§18.8: a successful fetch
//! returns "exactly the bytes identified by `$sha512`").
//!
//! [`BlobIntegrity`] pairs an expected digest with the verification, so a
//! `fetch` through an untrusted [`BlobConnector`] either returns hash-clean
//! bytes or a typed [`VerifiedFetchError`]. Whether the runtime then *recovers*
//! from another verified holder is a client MAY (SPEC-ISSUES item 19) and lives
//! above this crate.

use data_encoding::HEXLOWER;
use sha2::{Digest, Sha512 as Sha512Hasher};

use liasse_value::Sha512;

use crate::connector::{BlobConnector, ByteRange, ConnectorFailure};

/// The expected content digest of a blob, and the verification of bytes
/// against it (§18.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobIntegrity {
    expected: Sha512,
}

impl BlobIntegrity {
    /// A check for content whose canonical descriptor pins `expected`.
    #[must_use]
    pub fn new(expected: Sha512) -> Self {
        Self { expected }
    }

    /// The expected digest.
    #[must_use]
    pub fn expected(&self) -> &Sha512 {
        &self.expected
    }

    /// The canonical lowercase-hex SHA-512 of `bytes`.
    #[must_use]
    pub fn digest_hex(bytes: &[u8]) -> String {
        HEXLOWER.encode(&Sha512Hasher::digest(bytes))
    }

    /// Verify that `bytes` hash to the expected digest (§18.9). A mismatch is
    /// the observable "corrupt/tampered content" signal. Comparison is over the
    /// canonical hex text, so no fallible re-parse of the computed hash is
    /// needed.
    pub fn verify(&self, bytes: &[u8]) -> Result<(), IntegrityMismatch> {
        let actual = Self::digest_hex(bytes);
        if actual == self.expected.to_canonical_text() {
            Ok(())
        } else {
            Err(IntegrityMismatch {
                expected: self.expected.to_canonical_text(),
                actual,
            })
        }
    }

    /// Fetch the full object through `connector` and verify it before
    /// returning (§18.9 `fetch`). Tampered bytes never come back as `Ok`; they
    /// surface as [`VerifiedFetchError::Tampered`].
    pub fn fetch_verified(
        &self,
        connector: &dyn BlobConnector,
    ) -> Result<Vec<u8>, VerifiedFetchError> {
        let bytes = connector
            .fetch(&self.expected)
            .map_err(VerifiedFetchError::Connector)?;
        self.verify(&bytes).map_err(VerifiedFetchError::Tampered)?;
        Ok(bytes)
    }

    /// Fetch a byte range through `connector` (§18.12 range reads). A single
    /// range cannot be hash-verified against the whole-object digest; a caller
    /// that assembles the whole object verifies it with [`BlobIntegrity::verify`].
    pub fn fetch_range(
        &self,
        connector: &dyn BlobConnector,
        range: ByteRange,
    ) -> Result<Vec<u8>, VerifiedFetchError> {
        connector
            .fetch_range(&self.expected, range)
            .map_err(VerifiedFetchError::Connector)
    }
}

/// The content hashed to a different digest than the descriptor pins (§18.9),
/// carried as canonical hex text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("content integrity mismatch: expected `{expected}`, got `{actual}`")]
pub struct IntegrityMismatch {
    /// The digest the descriptor pins.
    pub expected: String,
    /// The digest the bytes actually hash to.
    pub actual: String,
}

/// A verified fetch failed either at the transport or at verification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifiedFetchError {
    /// The connector could not deliver the bytes (§18.12).
    #[error(transparent)]
    Connector(ConnectorFailure),
    /// The connector delivered bytes whose hash did not match — a compromised
    /// read transport. The runtime MUST NOT deliver these as a successful fetch
    /// (§18.9); recovery from another holder is a separate client MAY.
    #[error("connector delivered tampered content")]
    Tampered(IntegrityMismatch),
}
