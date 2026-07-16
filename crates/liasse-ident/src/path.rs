//! Display paths (Annex D.3).
//!
//! A display path alternates declaration-name segments and canonical key-text
//! segments and identifies a logical row (or a member within one) inside one
//! loaded package tree, e.g. `/companies/acme/offices/paris/rooms/main`. A
//! constructed [`CanonicalPath`] is valid by construction; the fallible entry
//! point is [`CanonicalPath::parse`], which splits an untrusted string.
//!
//! Parsing cannot classify a segment as a name or a key without the package
//! schema (a static-struct member and a key both surface as name-like text, so
//! the name/key alternation is not a strict positional rule). Parsing therefore
//! yields still-escaped [`RawSegment`]s that the caller resolves against its
//! schema via [`RawSegment::as_name`] or [`RawSegment::as_key`].

use crate::error::IdentError;
use crate::escape::Codec;
use crate::key::KeyText;

/// A declaration-name display-path segment (collection name, field name, module
/// mount…). Stored decoded; rendered with the D.3 name codec (`%` and `/`
/// escaped, `:` left literal).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NameSegment(String);

impl NameSegment {
    /// Wrap a raw declaration name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The raw (decoded) name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The escaped path segment text.
    #[must_use]
    pub fn encode(&self) -> String {
        Codec::NAME.encode(&self.0)
    }

    /// Decode an escaped name segment.
    pub fn decode(encoded: &str) -> Result<Self, IdentError> {
        Codec::NAME.decode(encoded).map(Self)
    }
}

/// One segment of a display path: a declaration name, or a key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PathSegment {
    Name(NameSegment),
    Key(KeyText),
}

impl PathSegment {
    /// The escaped text of this segment as it appears between path separators.
    #[must_use]
    pub fn encode(&self) -> String {
        match self {
            Self::Name(name) => name.encode(),
            Self::Key(key) => key.as_str().to_owned(),
        }
    }
}

/// A still-escaped display-path segment produced by [`CanonicalPath::parse`].
/// Its name-or-key role is resolved by the caller against the package schema.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RawSegment(String);

impl RawSegment {
    /// The escaped segment text, exactly as it appeared in the path.
    #[must_use]
    pub fn as_encoded(&self) -> &str {
        &self.0
    }

    /// Resolve this segment as a declaration name (decode the name escapes).
    pub fn as_name(&self) -> Result<NameSegment, IdentError> {
        NameSegment::decode(&self.0)
    }

    /// Resolve this segment as key text.
    pub fn as_key(&self) -> Result<KeyText, IdentError> {
        KeyText::parse(self.0.clone())
    }
}

/// A well-formed display path (D.3): an ordered list of typed segments.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalPath(Vec<PathSegment>);

impl CanonicalPath {
    /// Assemble a path from typed segments. Valid by construction.
    #[must_use]
    pub fn new(segments: impl IntoIterator<Item = PathSegment>) -> Self {
        Self(segments.into_iter().collect())
    }

    /// The typed segments in order.
    #[must_use]
    pub fn segments(&self) -> &[PathSegment] {
        &self.0
    }

    /// Render the canonical display path: a leading `/`, segments joined by `/`.
    /// The escaping guarantees no literal `/` appears inside a segment, so the
    /// rendering is unambiguously re-splittable.
    #[must_use]
    pub fn to_display_string(&self) -> String {
        let mut out = String::new();
        for segment in &self.0 {
            out.push('/');
            out.push_str(&segment.encode());
        }
        if out.is_empty() {
            out.push('/');
        }
        out
    }

    /// Split an untrusted display-path string into still-escaped segments.
    ///
    /// Rejects a path without a leading `/`, an empty (`//` or trailing-`/`)
    /// segment, and any malformed escape. `"/"` parses to zero segments (the
    /// tree root). Segments are returned unclassified — see [`RawSegment`].
    pub fn parse(text: &str) -> Result<Vec<RawSegment>, IdentError> {
        let body = text.strip_prefix('/').ok_or_else(|| IdentError::PathMissingRoot {
            text: text.to_owned(),
        })?;
        if body.is_empty() {
            return Ok(Vec::new());
        }
        body.split('/')
            .map(|segment| {
                if segment.is_empty() {
                    return Err(IdentError::EmptyPathSegment {
                        text: text.to_owned(),
                    });
                }
                Codec::KEY.validate(segment)?;
                Ok(RawSegment(segment.to_owned()))
            })
            .collect()
    }
}
