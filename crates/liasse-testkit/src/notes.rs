//! Extraction of a chapter's documented step keys from its `NOTES.md`.
//!
//! FORMAT.md: "The harness treats undocumented step keys as corpus errors."
//! A chapter documents each local step key it introduces (or reuses) in its
//! `NOTES.md`, invariably inside backticks — as a `### `key`` heading, a
//! `| `key` |` table cell, or a ``` ```hjson ``` code sample that opens with
//! `{ key: ... }`. This module harvests the leading identifier of every
//! backticked span, yielding the set of keys a case in that chapter may use.

use std::collections::BTreeSet;
use std::path::Path;

use crate::error::LoadError;

/// The step keys a chapter's `NOTES.md` documents.
#[derive(Debug, Clone, Default)]
pub struct ChapterNotes {
    keys: BTreeSet<String>,
}

impl ChapterNotes {
    /// Whether `key` is documented for this chapter.
    #[must_use]
    pub fn documents(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    /// The documented key set.
    #[must_use]
    pub fn keys(&self) -> &BTreeSet<String> {
        &self.keys
    }

    /// Harvest documented keys from `NOTES.md` text.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut keys = BTreeSet::new();
        for line in text.lines() {
            for (index, span) in line.split('`').enumerate() {
                if index % 2 == 1
                    && let Some(ident) = leading_identifier(span)
                {
                    keys.insert(ident);
                }
            }
        }
        Self { keys }
    }

    /// Read and parse the `NOTES.md` in `chapter_dir`. A missing file yields an
    /// empty set (a chapter that introduces no local step key needs no notes).
    pub fn load(chapter_dir: &Path) -> Result<Self, LoadError> {
        let path = chapter_dir.join("NOTES.md");
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(LoadError::Read { path, source: err }),
        }
    }
}

/// The leading `[a-z][a-z0-9_]{2,}` run of a backtick span, if any. Requiring a
/// lowercase-letter start and length three excludes prose fragments and short
/// modifier tokens while admitting every step key (the shortest is `erase`).
fn leading_identifier(span: &str) -> Option<String> {
    let mut chars = span.char_indices();
    let (_, first) = chars.next()?;
    if !first.is_ascii_lowercase() {
        return None;
    }
    let mut end = first.len_utf8();
    for (offset, ch) in chars {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' {
            end = offset + ch.len_utf8();
        } else {
            break;
        }
    }
    let ident = span.get(..end)?;
    if ident.len() >= 3 { Some(ident.to_owned()) } else { None }
}
