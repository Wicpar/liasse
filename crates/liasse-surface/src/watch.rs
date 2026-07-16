//! A live subscription over a surface view (SPEC.md §12.2).
//!
//! A subscription begins with a complete result and a frontier, then receives
//! ordered patches; after applying every patch the client result MUST equal the
//! authorized declared view at the new frontier (§12.2). This layer guarantees
//! that equality the direct way: it recomputes the view at each frontier through
//! the engine's [`ViewResult`] and emits the minimal [`ViewDelta`] between the
//! prior result and the new one — so the applied client result is, by
//! construction, the recomputed view.
//!
//! The runtime re-evaluates authorization and projection at every outgoing
//! frontier; when the state removes the subscription's authority the runtime
//! emits `close` (§12.2). The host decides authority and drives [`Watch::close`];
//! this type carries the view-tracking state and the close latch.

use liasse_runtime::{CommitSeq, ViewDelta, ViewResult};

/// The authorization context a subscription re-checks at every frontier: which
/// authentication context it belongs to (§11.8) and, for a role surface, the
/// role whose membership must still hold (§12.2).
#[derive(Debug, Clone)]
pub struct WatchAuthz {
    context: Option<String>,
    role: Option<String>,
}

impl WatchAuthz {
    /// A public subscription: no context, no role re-check.
    #[must_use]
    pub fn public() -> Self {
        Self { context: None, role: None }
    }

    /// A role subscription bound to authentication `context` and gated by `role`.
    #[must_use]
    pub fn role(context: impl Into<String>, role: impl Into<String>) -> Self {
        Self { context: Some(context.into()), role: Some(role.into()) }
    }

    /// The authentication context this subscription belongs to, if any.
    #[must_use]
    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    /// The role whose membership must still hold, if any.
    #[must_use]
    pub fn role_name(&self) -> Option<&str> {
        self.role.as_deref()
    }
}

/// A live subscription's tracked state.
pub struct Watch {
    view: String,
    authz: WatchAuthz,
    frontier: CommitSeq,
    last: Option<ViewResult>,
    closed: Option<String>,
}

impl Watch {
    /// Open a subscription over the runtime view `view`, at initial `frontier`,
    /// under `authz`, before any result has been delivered.
    #[must_use]
    pub fn open(view: impl Into<String>, authz: WatchAuthz, frontier: CommitSeq) -> Self {
        Self { view: view.into(), authz, frontier, last: None, closed: None }
    }

    /// The runtime view this subscription reads.
    #[must_use]
    pub fn view(&self) -> &str {
        &self.view
    }

    /// The subscription's authorization context.
    #[must_use]
    pub fn authz(&self) -> &WatchAuthz {
        &self.authz
    }

    /// The subscription's current frontier.
    #[must_use]
    pub fn frontier(&self) -> CommitSeq {
        self.frontier
    }

    /// The last delivered complete result, if the subscription has initialized.
    #[must_use]
    pub fn current(&self) -> Option<&ViewResult> {
        self.last.as_ref()
    }

    /// Whether the subscription has been closed, and why.
    #[must_use]
    pub fn close_reason(&self) -> Option<&str> {
        self.closed.as_deref()
    }

    /// Deliver the initial complete result at `frontier`, returning the `init`
    /// delta (§12.2). Called once, when the subscription opens.
    pub fn init(&mut self, result: ViewResult, frontier: CommitSeq) -> ViewDelta {
        let delta = ViewDelta::between(None, &result);
        self.last = Some(result);
        self.frontier = frontier;
        delta
    }

    /// Advance to `result` at `frontier`, returning the coherent patch delta from
    /// the prior result (§12.2). The applied client result equals `result` — the
    /// recomputed authorized view — by construction.
    pub fn advance(&mut self, result: ViewResult, frontier: CommitSeq) -> ViewDelta {
        let delta = ViewDelta::between(self.last.as_ref(), &result);
        self.last = Some(result);
        self.frontier = frontier;
        delta
    }

    /// Close the subscription at its current frontier with `reason` (§12.2). The
    /// cached result is released; no further deltas are delivered.
    pub fn close(&mut self, reason: impl Into<String>) {
        self.last = None;
        self.closed = Some(reason.into());
    }
}
