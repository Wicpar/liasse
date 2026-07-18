//! Small scalar newtypes: `text`, `bytes`, `uuid` (A.1).

use data_encoding::BASE64;

use crate::error::ValueError;

/// A Unicode text value (A.1), preserved exactly.
///
/// Text order (B.1) is Unicode scalar-value order, which for UTF-8 is byte
/// order — exactly [`String`]'s `Ord` — so no custom comparison is needed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Text(String);

impl Text {
    /// Wrap a string. Any Rust string is a valid Unicode scalar sequence, so
    /// this is total.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The exact text (A.1 "preserved exactly").
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume into the inner string.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// A small binary value (A.1). Ordering (B.1) is lexicographic unsigned-byte
/// order — [`Vec<u8>`]'s `Ord`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bytes(Vec<u8>);

impl Bytes {
    /// Wrap raw bytes.
    #[must_use]
    pub fn new(value: impl Into<Vec<u8>>) -> Self {
        Self(value.into())
    }

    /// The raw bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Decode canonical padded base64 (A.1 / D.2). The empty string decodes to
    /// the empty byte value — a present, valid value distinct from `none`.
    pub fn from_base64(text: &str) -> Result<Self, ValueError> {
        BASE64
            .decode(text.as_bytes())
            .map(Bytes)
            .map_err(|e| ValueError::MalformedBase64(e.to_string()))
    }

    /// Encode as canonical padded base64.
    #[must_use]
    pub fn to_base64(&self) -> String {
        BASE64.encode(&self.0)
    }
}

/// A 128-bit UUID (A.1). Ordering (B.1) is unsigned lexicographic order of its
/// 16 bytes — [`uuid::Uuid`]'s `Ord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uuid(uuid::Uuid);

impl Uuid {
    /// Parse a UUID leniently, normalizing to the canonical lowercase-hyphenated
    /// form (A.1). This is the **authoring** parse ([`Type::decode`](crate::Type::decode)):
    /// any case is accepted and canonicalized. The **wire** boundary
    /// ([`Type::decode_wire`](crate::Type::decode_wire)) rejects a non-canonical
    /// (e.g. uppercase) spelling instead of normalizing it (SPEC-ISSUES item 2).
    pub fn parse(text: &str) -> Result<Self, ValueError> {
        uuid::Uuid::parse_str(text)
            .map(Uuid)
            .map_err(|_| ValueError::MalformedUuid(text.to_owned()))
    }

    /// Build from the raw 16 bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(uuid::Uuid::from_bytes(bytes))
    }

    /// The canonical lowercase hyphenated string (A.1 / D.2).
    #[must_use]
    pub fn to_canonical_text(&self) -> String {
        self.0.as_hyphenated().to_string()
    }
}
