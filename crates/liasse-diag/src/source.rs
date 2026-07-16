//! The source registry: named source texts addressed by cheap, stable
//! [`SourceId`]s, and [`Span`], a [`ByteSpan`] located in one registered
//! source.

use crate::span::ByteSpan;
use core::ops::Range;

/// A stable, cheap-to-copy handle to a source registered in a [`SourceMap`].
///
/// Ids are dense indices assigned in insertion order; they stay valid for the
/// life of the map (sources are never removed). An id only resolves against the
/// map that issued it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(u32);

impl SourceId {
    /// The raw index, exposed for stable ordering and debugging only.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// How a source names itself when rendered.
///
/// A [`SourceName::File`] renders as its path verbatim; a
/// [`SourceName::Label`] is a synthetic origin (a REPL line, an inline string)
/// and renders bracketed as `<label>` so it never reads as a real file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceName {
    /// A real file path.
    File(String),
    /// A synthetic origin with no backing file.
    Label(String),
}

impl SourceName {
    /// The string shown in a rendered snippet header.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::File(path) => path.clone(),
            Self::Label(label) => {
                let mut out = String::with_capacity(label.len() + 2);
                out.push('<');
                out.push_str(label);
                out.push('>');
                out
            }
        }
    }
}

/// One registered source: its name and full text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    name: SourceName,
    text: String,
}

impl Source {
    /// The source's name.
    #[must_use]
    pub fn name(&self) -> &SourceName {
        &self.name
    }

    /// The full source text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The substring covered by `span`, or `None` if the span runs past the
    /// end of the text or lands off a UTF-8 char boundary.
    #[must_use]
    pub fn slice(&self, span: ByteSpan) -> Option<&str> {
        self.text.get(span.range())
    }

    /// The byte range of `span` within this text, clamped to the text length
    /// and widened outward to the nearest UTF-8 char boundaries.
    ///
    /// The result always indexes `text` safely: both bounds sit on char
    /// boundaries and lie within `0..=text.len()`. A span that overruns the
    /// text or splits a multi-byte char is absorbed rather than propagated, so
    /// a stale span can never make a rendering backend index off a boundary and
    /// panic. This widens to whole chars the way rustc does, so the annotation
    /// still covers the char the caller pointed inside of.
    #[must_use]
    pub fn char_boundary_range(&self, span: ByteSpan) -> Range<usize> {
        let text = self.text.as_str();
        let mut end = (span.end() as usize).min(text.len());
        let mut start = (span.start() as usize).min(end);
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        while end < text.len() && !text.is_char_boundary(end) {
            end += 1;
        }
        start..end
    }
}

/// A registry of named source texts.
///
/// Insertion returns a [`SourceId`] used to locate spans and to render them.
/// The map owns every source text for the life of a diagnostics session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceMap {
    sources: Vec<Source>,
}

impl SourceMap {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a real file and returns its stable id.
    pub fn add_file(&mut self, path: impl Into<String>, text: impl Into<String>) -> SourceId {
        self.insert(SourceName::File(path.into()), text.into())
    }

    /// Registers a synthetic (non-file) source and returns its stable id.
    pub fn add_label(&mut self, label: impl Into<String>, text: impl Into<String>) -> SourceId {
        self.insert(SourceName::Label(label.into()), text.into())
    }

    fn insert(&mut self, name: SourceName, text: String) -> SourceId {
        let id = SourceId(self.sources.len() as u32);
        self.sources.push(Source { name, text });
        id
    }

    /// The source behind `id`, or `None` if `id` was not issued by this map.
    #[must_use]
    pub fn get(&self, id: SourceId) -> Option<&Source> {
        self.sources.get(id.index() as usize)
    }

    /// The number of registered sources.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Whether the registry holds no sources.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// The text a located span points at, or `None` if the source is unknown
    /// or the span is out of range.
    #[must_use]
    pub fn span_text(&self, span: Span) -> Option<&str> {
        self.get(span.source())?.slice(span.bytes())
    }
}

/// A byte span located in one registered source: a [`ByteSpan`] plus the
/// [`SourceId`] it is measured against.
///
/// This is the span form carried by diagnostic labels. Pure offset math lives
/// on [`ByteSpan`]; [`Span`] adds only the source identity needed to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    source: SourceId,
    bytes: ByteSpan,
}

impl Span {
    /// Locates `bytes` in `source`.
    #[must_use]
    pub const fn new(source: SourceId, bytes: ByteSpan) -> Self {
        Self { source, bytes }
    }

    /// The source this span is measured against.
    #[must_use]
    pub const fn source(self) -> SourceId {
        self.source
    }

    /// The byte offsets, dropping the source identity.
    #[must_use]
    pub const fn bytes(self) -> ByteSpan {
        self.bytes
    }

    /// The convex hull of two spans in the *same* source, or `None` when they
    /// name different sources — merging across sources is a caller bug, not a
    /// silently-absorbed gap.
    #[must_use]
    pub fn merge(self, other: Self) -> Option<Self> {
        if self.source != other.source {
            return None;
        }
        Some(Self {
            source: self.source,
            bytes: self.bytes.merge(other.bytes),
        })
    }

    /// Whether `other` is the same source and lies within `self`.
    #[must_use]
    pub fn contains(self, other: Self) -> bool {
        self.source == other.source && self.bytes.contains(other.bytes)
    }
}
