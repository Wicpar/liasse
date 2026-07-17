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
//! occurrence later leaves the view, the window freezes the anchor's last complete
//! **sort tuple plus occurrence identity** as an immutable ordered gap coordinate
//! and tracks "the first rows at or after it" — a fixed position in the total sort
//! order, not a live neighbor — until the occurrence reappears (§12.2).
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
use liasse_value::Value;

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

/// The frozen ordered gap coordinate of a concrete anchor: the anchor's last
/// complete **sort tuple plus occurrence identity**, captured at the last frontier
/// its occurrence was present (§12.2). A sort tuple alone is not a position when
/// rows tie on it: the view's total order breaks such ties by occurrence identity
/// (§8, Annex B.5), so the gap retains the pair to name one exact position — a
/// fixed position, not a live neighbor — that decides where the window begins
/// while the occurrence is gone.
#[derive(Debug, Clone)]
struct FrozenGap {
    coordinate: Vec<Value>,
    occurrence: RowId,
}

impl FrozenGap {
    /// The window start while the anchor is absent: the first current row whose
    /// ordered position is at or after the frozen coordinate (§12.2). That position
    /// is the pair `(sort tuple, occurrence identity)` — the exact total order the
    /// engine's `order_rows` produces (sort keys, then [`RowId`] as the §8/B.5 final
    /// tiebreak) — so a `partition_point` on the pair fixes both the distinct-tuple
    /// case and the equal-sort-key tie case a bare sort tuple got wrong.
    fn resume(&self, rows: &[ViewRow]) -> usize {
        let frozen = (self.coordinate.as_slice(), &self.occurrence);
        rows.partition_point(|row| (row.sort_tuple(), row.id()) < frozen)
    }
}

/// A bounded window's request and the mutable coordinate it tracks (§12.2).
#[derive(Debug, Clone)]
pub struct Window {
    size: usize,
    anchor: Anchor,
    slide: bool,
    gap: Option<FrozenGap>,
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
            Anchor::At(occurrence) => match locate(rows, occurrence) {
                // Present: (re)freeze the immutable gap at the anchor's current
                // (sort tuple, occurrence identity) pair, then place the window
                // (§12.2, §8/B.5). The occurrence is the anchor's own, in hand.
                Some((index, row)) => {
                    self.gap = Some(FrozenGap {
                        coordinate: row.sort_tuple().to_vec(),
                        occurrence: occurrence.clone(),
                    });
                    if self.slide {
                        center(index, self.size, rows.len())
                    } else {
                        index
                    }
                }
                // Absent: the frozen (sort tuple, occurrence) coordinate holds the
                // window until the occurrence reappears (§12.2). No gap yet ⇒
                // unopenable.
                None => self.gap.as_ref()?.resume(rows),
            },
        };
        Some(slice(rows, start, self.size))
    }
}

/// The index and row whose identity is `id` — the reappearance match (§12.2).
fn locate<'r>(rows: &'r [ViewRow], id: &RowId) -> Option<(usize, &'r ViewRow)> {
    rows.iter().enumerate().find(|(_, row)| row.id() == id)
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
