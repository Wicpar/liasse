//! Bounded windows over a live subscription (SPEC.md §12.2).
//!
//! A subscription MAY keep only a bounded slice of a large view incremental. A
//! window is a client-side projection over the full authorized [`ViewResult`] the
//! engine recomputes at each frontier: it never changes what the surface reads,
//! only how many rows — and which — the client tracks.
//!
//! ```text
//! { $size: n }                                    first n rows
//! { $size: n, $anchor: $first | $last }           first / last n rows
//! { $size: n, $anchor: occurrence }               n rows from the anchor
//! { $size: n, $anchor: occurrence, $slide: true } the anchor centered
//! ```
//!
//! `$size` is a non-negative row count (§12.2), so a zero-row window is a valid,
//! permanently empty, still-live subscription. A concrete anchor MUST identify
//! exactly one current occurrence when the window opens ([`WindowError`]); row
//! identity is unique within a result, so "exactly one" is "present". If that
//! occurrence later leaves the view, the window freezes its ordered neighbor
//! coordinate as an immutable gap and tracks "the first rows at or after it" until
//! the occurrence reappears (§12.2).
//!
//! # Runtime seam
//!
//! The anchor follows an *occurrence*, matched here by the engine's
//! [`RowId`] — which is key-derived (Annex D.1). An atomic rekey therefore changes
//! the tracked identity, so following one occurrence *across a rekey* (§12.2
//! "`rekey` ... preserving occurrence") needs a rekey-stable occurrence identity
//! the current [`ViewResult`] does not carry; that case is left to the runtime.

use liasse_expr::RowId;
use liasse_runtime::{ViewResult, ViewRow};

/// Where a bounded window sits within the underlying view (§12.2).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Anchor {
    /// The first rows (the no-anchor default and `$anchor: $first`).
    First,
    /// The last rows (`$anchor: $last`).
    Last,
    /// One concrete row occurrence, by its stable identity.
    At(RowId),
}

/// The frozen ordered neighbor coordinate of a concrete anchor, captured at the
/// last frontier its occurrence was present (§12.2 "last complete sort tuple").
/// While the occurrence is gone this coordinate — not a live position — decides
/// where the window begins.
#[derive(Debug, Clone)]
struct Gap {
    left: Option<RowId>,
    right: Option<RowId>,
}

/// A bounded window's request and the mutable coordinate it tracks (§12.2).
#[derive(Debug, Clone)]
pub struct Window {
    size: usize,
    anchor: Anchor,
    slide: bool,
    gap: Option<Gap>,
}

/// A window that could not open: its concrete anchor identified no current
/// occurrence, violating the §12.2 "exactly one current occurrence" requirement.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("window anchor identifies zero current occurrences at window open")]
pub struct WindowError;

impl Window {
    /// A first-`size`-rows window (the no-anchor default, §12.2).
    #[must_use]
    pub fn first(size: usize) -> Self {
        Self { size, anchor: Anchor::First, slide: false, gap: None }
    }

    /// A last-`size`-rows window (`$anchor: $last`).
    #[must_use]
    pub fn last(size: usize) -> Self {
        Self { size, anchor: Anchor::Last, slide: false, gap: None }
    }

    /// A window of `size` rows anchored on the occurrence identified by `anchor`.
    /// The anchor normally becomes the first row; [`Window::sliding`] centers it.
    #[must_use]
    pub fn anchored(size: usize, anchor: RowId) -> Self {
        Self { size, anchor: Anchor::At(anchor), slide: false, gap: None }
    }

    /// Center the concrete anchor within the window as far as the view bounds
    /// allow (`$slide: true`). No effect on a first/last window.
    #[must_use]
    pub fn sliding(mut self) -> Self {
        self.slide = true;
        self
    }

    /// Open the window over `result`, returning its initial rows. A concrete
    /// anchor MUST resolve to one current occurrence (§12.2).
    ///
    /// # Errors
    /// [`WindowError`] when a concrete anchor identifies no current occurrence.
    pub fn open(&mut self, result: &ViewResult) -> Result<Vec<ViewRow>, WindowError> {
        self.select(result.rows()).ok_or(WindowError)
    }

    /// Recompute the window over `result` at a new frontier. Once the window has
    /// opened, a concrete anchor that has since left the view is tracked through
    /// its frozen gap coordinate, so this always yields rows.
    #[must_use]
    pub fn refresh(&mut self, result: &ViewResult) -> Vec<ViewRow> {
        self.select(result.rows()).unwrap_or_default()
    }

    /// The window's start index into `rows`, or `None` only for a concrete anchor
    /// whose occurrence is absent and whose gap has not yet been frozen (an
    /// unopenable window). Refreshes the frozen gap whenever the occurrence shows.
    fn select(&mut self, rows: &[ViewRow]) -> Option<Vec<ViewRow>> {
        let start = match &self.anchor {
            Anchor::First => 0,
            Anchor::Last => rows.len().saturating_sub(self.size),
            Anchor::At(occurrence) => match position(rows, occurrence) {
                Some(index) => {
                    self.gap = Some(Gap {
                        left: index
                            .checked_sub(1)
                            .and_then(|i| rows.get(i))
                            .map(|row| row.id().clone()),
                        right: rows.get(index + 1).map(|row| row.id().clone()),
                    });
                    if self.slide {
                        center(index, self.size, rows.len())
                    } else {
                        index
                    }
                }
                None => gap_start(rows, self.gap.as_ref()?),
            },
        };
        Some(slice(rows, start, self.size))
    }
}

/// The index of the row whose identity is `id`, if present.
fn position(rows: &[ViewRow], id: &RowId) -> Option<usize> {
    rows.iter().position(|row| row.id() == id)
}

/// `size` rows of `rows` beginning at `start`, saturating at the end.
fn slice(rows: &[ViewRow], start: usize, size: usize) -> Vec<ViewRow> {
    rows.iter().skip(start).take(size).cloned().collect()
}

/// The start index that centers a `size`-row window on the anchor at `index`,
/// clamped so the window never runs past either view bound (§12.2 `$slide`).
fn center(index: usize, size: usize, len: usize) -> usize {
    let max_start = len.saturating_sub(size);
    index.saturating_sub(size / 2).min(max_start)
}

/// The start index for the "first rows at or after the gap coordinate" (§12.2):
/// just past the frozen left neighbor if it survives, else at the frozen right
/// neighbor, else the view start when both have gone.
fn gap_start(rows: &[ViewRow], gap: &Gap) -> usize {
    if let Some(left) = &gap.left
        && let Some(index) = position(rows, left)
    {
        return index + 1;
    }
    if let Some(right) = &gap.right
        && let Some(index) = position(rows, right)
    {
        return index;
    }
    0
}
