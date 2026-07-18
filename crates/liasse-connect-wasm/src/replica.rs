//! The connection-level §12.2 replica the untrusted web client keeps.
//!
//! One logical connection carries the downstream frames of many subscriptions on a
//! single SSE stream; the frontier token that stamps each advance rides the SSE
//! `id:`, not the frame body ([`liasse_wire::Downstream::Frontier`] is empty for
//! that reason). [`ClientReplica`] is therefore the connection-level multiplexer:
//! it owns one [`WireStore`] per `sub`, routes each decoded frame to the right one,
//! and folds it in through the shared [`liasse_wire::apply`]. It holds NO authority
//! — it only relabels and moves the already-authorized projection — and every
//! transition is total: a frame that does not fit returns a [`CoreError`] rather
//! than corrupting the replica or panicking (AGENTS.md).
//!
//! The apply/marshal split keeps one source of truth: the §12.2 semantics live in
//! `liasse-wire`; this type only demultiplexes by `sub` and reports the effect. The
//! wasm-bindgen boundary marshals [`Applied`] to a `JsValue`; native tests read it
//! directly.

use std::collections::BTreeMap;

use serde::Serialize;

use liasse_wire::{
    CloseReason, Downstream, FaultCode, Ft, ResetReason, Sub, Value, WireRow, WireStore, decode,
};

use crate::error::CoreError;

/// A client's per-connection replica: the live subscriptions and the connection
/// frontier last observed. A closed/reset subscription stays in the map (its
/// terminal state is observable) until the connection is dropped or reset.
#[derive(Debug, Default)]
pub struct ClientReplica {
    subs: BTreeMap<Sub, WireStore>,
    frontier: Option<Ft>,
}

impl ClientReplica {
    /// A fresh replica with no subscriptions and no observed frontier.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode one downstream frame and fold it into the addressed subscription,
    /// advancing the retained frontier from the SSE `id:` (`frontier`, empty when the
    /// frame carries none). Returns what the frame did, so the shell can render the
    /// affected subscription without re-reading the whole replica.
    ///
    /// The frame is parsed as hostile input; nothing here trusts a token or panics.
    ///
    /// # Errors
    /// [`CoreError::Codec`] if `data` is not a well-formed downstream frame,
    /// [`CoreError::NotSubscribed`] if a `patch` names an unopened subscription, or
    /// [`CoreError::Store`] if the frame does not fit the subscription's state.
    pub fn apply(&mut self, data: &str, frontier: &str) -> Result<Applied, CoreError> {
        let frame: Downstream = decode(data)?;
        let ft = Ft::new(frontier);
        match frame {
            Downstream::Init { sub, rows } => {
                self.subs.entry(sub.clone()).or_default().init(rows, ft.clone())?;
                self.frontier = Some(ft.clone());
                Ok(Applied::rows(AppliedKind::Init, sub.clone(), ft, self.rows_of(&sub)))
            }
            Downstream::Scalar { sub, value } => {
                self.subs.entry(sub.clone()).or_default().scalar(value, ft.clone())?;
                self.frontier = Some(ft.clone());
                let scalar = self.scalar_of(&sub);
                Ok(Applied::scalar(AppliedKind::Scalar, sub, ft, scalar))
            }
            Downstream::Patch { sub, ops } => {
                {
                    let store = self
                        .subs
                        .get_mut(&sub)
                        .ok_or_else(|| CoreError::NotSubscribed(sub.as_str().to_owned()))?;
                    store.patch(&ops, ft.clone())?;
                }
                self.frontier = Some(ft.clone());
                Ok(Applied::rows(AppliedKind::Patch, sub.clone(), ft, self.rows_of(&sub)))
            }
            Downstream::Close { sub, reason } => {
                self.subs.entry(sub.clone()).or_default().close(reason);
                if !frontier.is_empty() {
                    self.frontier = Some(ft);
                }
                Ok(Applied::close(sub, reason))
            }
            Downstream::Frontier => {
                for store in self.subs.values_mut() {
                    // A terminated subscription cannot advance; ignoring its refusal
                    // keeps a connection-wide frontier ping total.
                    let _ = store.advance_frontier(ft.clone());
                }
                self.frontier = Some(ft.clone());
                Ok(Applied::connection(AppliedKind::Frontier, ft))
            }
            Downstream::Reset { reason } => {
                // The retained frontier is no longer replayable; drop the replica so
                // the fresh `init` that follows re-establishes each subscription.
                self.subs.clear();
                self.frontier = None;
                Ok(Applied::reset(reason))
            }
            Downstream::Fault { code, message } => Ok(Applied::fault(code, message)),
        }
    }

    /// The rows a subscription currently holds, in view order (empty for a pending,
    /// scalar, terminated, or unknown subscription).
    #[must_use]
    pub fn rows_of(&self, sub: &Sub) -> Vec<WireRow> {
        self.subs.get(sub).map(|store| store.rows().to_vec()).unwrap_or_default()
    }

    /// The scalar value of a subscription, or `None` for any other shape.
    #[must_use]
    pub fn scalar_of(&self, sub: &Sub) -> Option<Value> {
        self.subs.get(sub).and_then(|store| store.scalar_value().cloned())
    }

    /// The frontier a subscription was last observed at.
    #[must_use]
    pub fn frontier_of(&self, sub: &Sub) -> Option<Ft> {
        self.subs.get(sub).and_then(|store| store.frontier().cloned())
    }

    /// Whether a subscription has terminated (closed or reset). An unknown
    /// subscription is not closed — it was simply never opened.
    #[must_use]
    pub fn is_closed(&self, sub: &Sub) -> bool {
        self.subs.get(sub).is_some_and(|store| !store.is_live())
    }

    /// The reason a subscription closed, if it did.
    #[must_use]
    pub fn close_reason_of(&self, sub: &Sub) -> Option<CloseReason> {
        self.subs.get(sub).and_then(WireStore::close_reason)
    }

    /// The subscriptions the replica is tracking, in `sub` order.
    #[must_use]
    pub fn subs(&self) -> Vec<Sub> {
        self.subs.keys().cloned().collect()
    }

    /// The connection frontier last observed across all subscriptions.
    #[must_use]
    pub fn connection_frontier(&self) -> Option<&Ft> {
        self.frontier.as_ref()
    }
}

/// What kind of downstream frame [`ClientReplica::apply`] folded in. Named so the
/// shell can branch on the effect without re-parsing the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppliedKind {
    /// A row-stream subscription's initial set arrived.
    Init,
    /// A scalar subscription's value arrived.
    Scalar,
    /// A row-stream subscription advanced by a patch.
    Patch,
    /// A subscription was closed.
    Close,
    /// The whole connection advanced its frontier with no result change.
    Frontier,
    /// The connection was reset; the replica was dropped.
    Reset,
    /// A transport fault arrived; no state changed.
    Fault,
}

/// A transport fault reported to the client (never model state): a stable class and
/// a sanitized message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fault {
    /// The stable fault class.
    pub code: FaultCode,
    /// The sanitized human-readable description.
    pub message: String,
}

/// The client-visible effect of applying one downstream frame. Absent members are
/// omitted so the marshaled JS object carries only what the frame produced.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Applied {
    /// Which frame was applied.
    pub kind: AppliedKind,
    /// The subscription affected, for a per-subscription frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    /// The connection frontier after applying the frame, when it advanced one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frontier: Option<String>,
    /// The affected subscription's rows after the frame (row streams only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<WireRow>>,
    /// The affected subscription's scalar value after the frame (scalar views only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scalar: Option<Value>,
    /// Why the subscription closed, on a `close`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<CloseReason>,
    /// Why the connection reset, on a `reset`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_reason: Option<ResetReason>,
    /// The transport fault, on a `fault`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault: Option<Fault>,
}

impl Applied {
    fn base(kind: AppliedKind) -> Self {
        Self {
            kind,
            sub: None,
            frontier: None,
            rows: None,
            scalar: None,
            close_reason: None,
            reset_reason: None,
            fault: None,
        }
    }

    fn rows(kind: AppliedKind, sub: Sub, ft: Ft, rows: Vec<WireRow>) -> Self {
        Self {
            sub: Some(sub.into_inner()),
            frontier: Some(ft.into_inner()),
            rows: Some(rows),
            ..Self::base(kind)
        }
    }

    fn scalar(kind: AppliedKind, sub: Sub, ft: Ft, scalar: Option<Value>) -> Self {
        Self {
            sub: Some(sub.into_inner()),
            frontier: Some(ft.into_inner()),
            scalar,
            ..Self::base(kind)
        }
    }

    fn connection(kind: AppliedKind, ft: Ft) -> Self {
        Self { frontier: Some(ft.into_inner()), ..Self::base(kind) }
    }

    fn close(sub: Sub, reason: CloseReason) -> Self {
        Self {
            sub: Some(sub.into_inner()),
            close_reason: Some(reason),
            ..Self::base(AppliedKind::Close)
        }
    }

    fn reset(reason: ResetReason) -> Self {
        Self { reset_reason: Some(reason), ..Self::base(AppliedKind::Reset) }
    }

    fn fault(code: FaultCode, message: String) -> Self {
        Self { fault: Some(Fault { code, message }), ..Self::base(AppliedKind::Fault) }
    }
}
