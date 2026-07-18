//! The client-side state of one subscription: the retained result, the frontier it
//! was last observed at, and the occurrence tokens currently present.
//!
//! A [`WireStore`] is the §12.2 replica a client keeps for one `sub`. It consumes
//! the downstream frames addressed to that subscription and folds them into the
//! current result via the shared [`crate::apply`], so the client's view stays equal
//! to the authorized view at each frontier without ever re-fetching. It holds NO
//! authority — it is a convenience replica (AGENTS.md) — and every transition is
//! total: a frame that does not fit the subscription's state returns a
//! [`StoreError`] rather than corrupting it.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::frame::{CloseReason, ResetReason};
use crate::patch::{ApplyError, PatchOp, apply};
use crate::row::WireRow;
use crate::token::{Ft, Occ};

/// A subscription's observed state. The live states carry the result (a row stream
/// or a scalar); the terminal states carry why the subscription ended. Modeling it
/// as one enum keeps invalid combinations — rows AND a scalar, or a patch onto a
/// closed subscription — unrepresentable.
#[derive(Debug, Clone, PartialEq)]
enum Phase {
    /// No `init`/`scalar` has arrived yet.
    Pending,
    /// A row-stream subscription and its current rows, in view order.
    Rows(Vec<WireRow>),
    /// A scalar/aggregate subscription and its current value.
    Scalar(Value),
    /// The server closed the subscription.
    Closed(CloseReason),
    /// The connection was reset; the client must re-view.
    Reset(ResetReason),
}

/// Why a downstream frame could not be folded into a subscription's state.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// A `patch` arrived before any `init` established the row set.
    #[error("cannot apply a patch before the subscription is initialized")]
    NotInitialized,
    /// A row frame arrived for a scalar subscription, or a scalar frame for a row
    /// subscription — a subscription's result shape is fixed for its lifetime.
    #[error("frame does not match the subscription's established result shape")]
    ShapeMismatch,
    /// A frame arrived after the subscription was closed or reset.
    #[error("the subscription is no longer live")]
    NotLive,
    /// A `patch` did not apply to the current result (see [`ApplyError`]).
    #[error(transparent)]
    Apply(#[from] ApplyError),
}

/// A client's replica of one subscription (§12.2).
#[derive(Debug, Clone, PartialEq)]
pub struct WireStore {
    phase: Phase,
    frontier: Option<Ft>,
}

impl Default for WireStore {
    fn default() -> Self {
        Self::new()
    }
}

impl WireStore {
    /// A fresh, uninitialized subscription replica.
    #[must_use]
    pub fn new() -> Self {
        Self { phase: Phase::Pending, frontier: None }
    }

    /// Take the initial row set of a row-stream subscription at `frontier` (§12.2
    /// `init`). Establishes — or, on a guarded shape change, re-establishes — the
    /// subscription as a row stream.
    ///
    /// # Errors
    /// [`StoreError::NotLive`] if the subscription was already closed or reset.
    pub fn init(&mut self, rows: Vec<WireRow>, frontier: Ft) -> Result<(), StoreError> {
        self.ensure_live()?;
        self.phase = Phase::Rows(rows);
        self.frontier = Some(frontier);
        Ok(())
    }

    /// Take a scalar/aggregate subscription's value at `frontier` (§7.5, §12.2).
    ///
    /// # Errors
    /// [`StoreError::NotLive`] if the subscription was already closed or reset.
    pub fn scalar(&mut self, value: Value, frontier: Ft) -> Result<(), StoreError> {
        self.ensure_live()?;
        self.phase = Phase::Scalar(value);
        self.frontier = Some(frontier);
        Ok(())
    }

    /// Fold an ordered §12.2 patch into the current row set and advance the retained
    /// frontier. An empty `ops` is the frontier-only patch: the rows are unchanged
    /// and only the frontier moves.
    ///
    /// The result is computed before any state is mutated, so a rejected patch
    /// leaves the subscription exactly as it was.
    ///
    /// # Errors
    /// [`StoreError::NotLive`] on a closed/reset subscription,
    /// [`StoreError::NotInitialized`] before the first `init`,
    /// [`StoreError::ShapeMismatch`] on a scalar subscription, or
    /// [`StoreError::Apply`] if the patch does not apply.
    pub fn patch(&mut self, ops: &[PatchOp], frontier: Ft) -> Result<(), StoreError> {
        self.ensure_live()?;
        match &self.phase {
            Phase::Rows(rows) => {
                let next = apply(rows, ops)?;
                self.phase = Phase::Rows(next);
                self.frontier = Some(frontier);
                Ok(())
            }
            Phase::Scalar(_) => Err(StoreError::ShapeMismatch),
            Phase::Pending => Err(StoreError::NotInitialized),
            Phase::Closed(_) | Phase::Reset(_) => Err(StoreError::NotLive),
        }
    }

    /// Advance the retained frontier without changing the result — a frontier-only
    /// downstream `frontier` frame for the whole connection.
    ///
    /// # Errors
    /// [`StoreError::NotLive`] if the subscription was already closed or reset.
    pub fn advance_frontier(&mut self, frontier: Ft) -> Result<(), StoreError> {
        self.ensure_live()?;
        self.frontier = Some(frontier);
        Ok(())
    }

    /// Record that the server closed the subscription (§12.2). Terminal.
    pub fn close(&mut self, reason: CloseReason) {
        self.phase = Phase::Closed(reason);
    }

    /// Record that the connection was reset (§12.2). Terminal; the client re-views.
    pub fn reset(&mut self, reason: ResetReason) {
        self.phase = Phase::Reset(reason);
    }

    /// The current rows of a row-stream subscription, in view order. Empty for a
    /// pending, scalar, or terminated subscription.
    #[must_use]
    pub fn rows(&self) -> &[WireRow] {
        match &self.phase {
            Phase::Rows(rows) => rows,
            _ => &[],
        }
    }

    /// The current value of a scalar subscription, or `None` for any other shape.
    #[must_use]
    pub fn scalar_value(&self) -> Option<&Value> {
        match &self.phase {
            Phase::Scalar(value) => Some(value),
            _ => None,
        }
    }

    /// The frontier the subscription was last observed at, if it has been observed.
    #[must_use]
    pub fn frontier(&self) -> Option<&Ft> {
        self.frontier.as_ref()
    }

    /// The occurrence tokens currently present in the result — empty unless this is
    /// a live row stream. A client uses it to reject a patch that targets an
    /// occurrence it does not hold before folding it in.
    #[must_use]
    pub fn known_occ(&self) -> BTreeSet<Occ> {
        self.rows().iter().map(|row| row.occ().clone()).collect()
    }

    /// Whether the subscription is still live (neither closed nor reset).
    #[must_use]
    pub fn is_live(&self) -> bool {
        matches!(self.phase, Phase::Pending | Phase::Rows(_) | Phase::Scalar(_))
    }

    /// The reason the subscription closed, if it did.
    #[must_use]
    pub fn close_reason(&self) -> Option<CloseReason> {
        match self.phase {
            Phase::Closed(reason) => Some(reason),
            _ => None,
        }
    }

    /// The reason the connection reset, if it did.
    #[must_use]
    pub fn reset_reason(&self) -> Option<ResetReason> {
        match self.phase {
            Phase::Reset(reason) => Some(reason),
            _ => None,
        }
    }

    /// Reject any transition on a terminated subscription.
    fn ensure_live(&self) -> Result<(), StoreError> {
        if self.is_live() {
            Ok(())
        } else {
            Err(StoreError::NotLive)
        }
    }
}
