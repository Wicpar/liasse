//! Byte spans: half-open `[start, end)` ranges of byte offsets into a source
//! text. A [`ByteSpan`] carries no source identity, so its offset math (merge,
//! containment) is total and cheap. Attach a source with [`crate::Span`] only
//! at the diagnostic boundary.

use core::ops::Range;

/// A half-open range of byte offsets `[start, end)` into some source text.
///
/// The type enforces `start <= end`, so [`ByteSpan::len`] never underflows and
/// [`ByteSpan::contains`] / [`ByteSpan::merge`] are well defined for every
/// value. Offsets are byte offsets, not char offsets: a span over a multi-byte
/// UTF-8 grapheme spans all of its bytes.
///
/// It is 8 bytes and `Copy`; combine spans freely without allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ByteSpan {
    start: u32,
    end: u32,
}

impl ByteSpan {
    /// Builds a span from explicit bounds, or `None` when `end < start`.
    ///
    /// Returning `None` rather than silently swapping keeps a reversed range —
    /// always a caller bug — from being mistaken for a valid empty or forward
    /// span. Use [`ByteSpan::cover`] when the order of two offsets is unknown.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Option<Self> {
        if end < start {
            None
        } else {
            Some(Self { start, end })
        }
    }

    /// The forward span covering both offsets, regardless of their order.
    #[must_use]
    pub const fn cover(a: u32, b: u32) -> Self {
        if a <= b {
            Self { start: a, end: b }
        } else {
            Self { start: b, end: a }
        }
    }

    /// A span beginning at `start` and running `len` bytes.
    #[must_use]
    pub const fn at(start: u32, len: u32) -> Self {
        Self {
            start,
            end: start.saturating_add(len),
        }
    }

    /// The empty span (a zero-width point) at `offset`.
    #[must_use]
    pub const fn point(offset: u32) -> Self {
        Self {
            start: offset,
            end: offset,
        }
    }

    /// The first byte offset in the span.
    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    /// The offset one past the last byte in the span.
    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }

    /// The width of the span in bytes.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    /// Whether the span is zero-width (a point).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The smallest span containing both operands (their convex hull).
    ///
    /// Total: gaps between the operands are absorbed, so merging disjoint spans
    /// yields the span that brackets them.
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        let start = if self.start <= other.start {
            self.start
        } else {
            other.start
        };
        let end = if self.end >= other.end {
            self.end
        } else {
            other.end
        };
        Self { start, end }
    }

    /// Whether `other` lies entirely within `self` (bounds inclusive).
    ///
    /// A point at either boundary, and any span equal to `self`, is contained.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    /// Whether the byte offset `offset` falls within `[start, end)`.
    ///
    /// A point span contains no offset, matching the half-open convention.
    #[must_use]
    pub const fn contains_offset(self, offset: u32) -> bool {
        self.start <= offset && offset < self.end
    }

    /// The offsets as a [`Range`], for slicing source text or feeding a
    /// rendering backend.
    #[must_use]
    pub fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}
