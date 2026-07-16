//! Spec anchors cited by a case's `spec:` and `violates:` arrays.
//!
//! FORMAT.md fixes the citation convention: a chapter `#anchor` precedes the
//! `§section` refs it introduces, and worked-example anchors (`W1`..`W4`) name
//! informative illustrations. This module keeps the citation text verbatim and
//! classifies it so downstream tooling can group by chapter without re-parsing.

use std::fmt;

/// One entry in a `spec:` or `violates:` array.
///
/// The text is preserved exactly (`"#state-model"`, `"§5.4"`, `"W1"`); the
/// [`AnchorKind`] classification is derived from its leading character.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpecAnchor(String);

/// The three shapes a [`SpecAnchor`] takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnchorKind {
    /// A chapter anchor such as `#state-model`.
    Chapter,
    /// A section reference such as `§5.4` or `§A.1`.
    Section,
    /// A worked-example or other bare label such as `W1`.
    Label,
}

impl SpecAnchor {
    /// Wrap a raw citation.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The citation text exactly as authored.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Classify the citation by its leading marker.
    #[must_use]
    pub fn kind(&self) -> AnchorKind {
        match self.0.chars().next() {
            Some('#') => AnchorKind::Chapter,
            Some('\u{a7}') => AnchorKind::Section,
            _ => AnchorKind::Label,
        }
    }
}

impl fmt::Display for SpecAnchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
