//! Integrity digests (Annex D.4 / D.5 / D.7).
//!
//! Annex D pins SHA-256 as the integrity primitive: the definition identifier
//! is "SHA-256 over the canonical bytes of `liasse.json`" (D.4), the manifest
//! records "a SHA-256 checksum" per entry (D.5), and an erasure stub carries
//! `sha256:<digest of original canonical typed value>` (D.7). (SHA-512 is a
//! separate concern — it hashes blob *content* and lives in `liasse-value`.)
//! The canonical textual form is `sha256:` followed by 64 lowercase hex digits,
//! exactly as the D.5 CBOR (`"definition": "sha256:..."`) and D.7 stub show.

use data_encoding::{HEXLOWER, HEXLOWER_PERMISSIVE};
use sha2::{Digest as _, Sha256};

use crate::error::IdentError;

/// A SHA-256 digest over canonical bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest([u8; 32]);

impl Digest {
    /// Compute the SHA-256 of the given canonical bytes.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let mut out = [0u8; 32];
        out.copy_from_slice(&hasher.finalize());
        Self(out)
    }

    /// Wrap raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The 32 raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parse the canonical `sha256:<hex>` textual form.
    ///
    /// The prefix is required. Hex case on input is not pinned for SHA-256
    /// (SPEC-ISSUES item 20 pins only the SHA-512 blob case); the
    /// least-surprising decoder accepts either case and the canonical output is
    /// always lowercase, so a round-tripped digest renders canonically.
    pub fn parse(text: &str) -> Result<Self, IdentError> {
        let hex = text.strip_prefix("sha256:").ok_or_else(|| IdentError::MalformedDigest {
            detail: format!("expected a `sha256:` prefix: `{text}`"),
        })?;
        let bytes = HEXLOWER_PERMISSIVE
            .decode(hex.as_bytes())
            .map_err(|e| IdentError::MalformedDigest {
                detail: format!("invalid hex: {e}"),
            })?;
        let array: [u8; 32] = bytes.try_into().map_err(|_| IdentError::MalformedDigest {
            detail: format!("expected 32 digest bytes (64 hex chars): `{text}`"),
        })?;
        Ok(Self(array))
    }

    /// The canonical `sha256:<64 lowercase hex>` text.
    #[must_use]
    pub fn to_canonical_text(&self) -> String {
        format!("sha256:{}", HEXLOWER.encode(&self.0))
    }
}

/// The canonical definition identifier (D.4): SHA-256 over the canonical bytes
/// of `liasse.json`. A distinct newtype from a bare [`Digest`] so a definition
/// identity cannot be confused with an arbitrary entry checksum; it shares the
/// same `sha256:` canonical text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefinitionId(Digest);

impl DefinitionId {
    /// Compute a definition identifier from the canonical `liasse.json` bytes.
    #[must_use]
    pub fn of_canonical_bytes(bytes: &[u8]) -> Self {
        Self(Digest::of_bytes(bytes))
    }

    /// Wrap an already-computed digest as a definition identifier.
    #[must_use]
    pub const fn from_digest(digest: Digest) -> Self {
        Self(digest)
    }

    /// The underlying digest.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.0
    }

    /// Parse the canonical `sha256:<hex>` textual form.
    pub fn parse(text: &str) -> Result<Self, IdentError> {
        Digest::parse(text).map(Self)
    }

    /// The canonical `sha256:<64 lowercase hex>` text.
    #[must_use]
    pub fn to_canonical_text(&self) -> String {
        self.0.to_canonical_text()
    }
}
