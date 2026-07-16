//! `blob` — binary-content descriptor (A.1, Blobs §, B.4).

use data_encoding::{HEXLOWER, HEXLOWER_PERMISSIVE};

use crate::error::ValueError;

/// A 64-byte SHA-512 content hash. Canonical text is 128 lowercase hex chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha512([u8; 64]);

impl Sha512 {
    /// Decode a hex SHA-512.
    ///
    /// The canonical output is lowercase (Blobs §); uppercase-hex input is
    /// unpinned (SPEC-ISSUES item 20), so we take the least-surprising decoder
    /// stance and accept either case, normalizing to lowercase.
    pub fn parse(text: &str) -> Result<Self, ValueError> {
        let bytes = HEXLOWER_PERMISSIVE
            .decode(text.as_bytes())
            .map_err(|e| ValueError::MalformedSha512(e.to_string()))?;
        let array: [u8; 64] = bytes
            .try_into()
            .map_err(|_| ValueError::MalformedSha512(format!("expected 64 bytes: {text}")))?;
        Ok(Self(array))
    }

    /// The canonical lowercase-hex string.
    #[must_use]
    pub fn to_canonical_text(&self) -> String {
        HEXLOWER.encode(&self.0)
    }
}

/// A canonical media type (e.g. `application/pdf`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MediaType(String);

impl MediaType {
    /// Wrap a media-type string.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The media-type string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A blob descriptor: the complete application value for binary content.
///
/// Two descriptors MAY name the same content (`sha512`) yet differ in media or
/// name; equality and ordering (B.4) therefore span the whole descriptor, in
/// the order `sha512`, `bytes`, `media`, then optional `name`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobDescriptor {
    sha512: Sha512,
    bytes: u64,
    media: MediaType,
    name: Option<String>,
}

impl BlobDescriptor {
    /// Assemble a descriptor from its verified components.
    #[must_use]
    pub fn new(sha512: Sha512, bytes: u64, media: MediaType, name: Option<String>) -> Self {
        Self {
            sha512,
            bytes,
            media,
            name,
        }
    }

    /// The content hash.
    #[must_use]
    pub fn sha512(&self) -> &Sha512 {
        &self.sha512
    }

    /// The byte count.
    #[must_use]
    pub const fn byte_count(&self) -> u64 {
        self.bytes
    }

    /// The media type.
    #[must_use]
    pub fn media(&self) -> &MediaType {
        &self.media
    }

    /// The optional file name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}
