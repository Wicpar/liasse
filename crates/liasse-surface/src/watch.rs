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

use std::collections::BTreeMap;

use liasse_runtime::{CommitSeq, ViewDelta, ViewResult, ViewRow};
use liasse_value::Value;

use crate::request::AuthSelection;
use crate::window::{Window, WindowError};

/// The authorization context a subscription re-checks at every frontier: which
/// authentication context it belongs to (§11.8) and, for a role surface, the
/// role whose membership must still hold (§12.2).
///
/// A role subscription re-authorizes at each outgoing frontier from either the
/// connection's stored context (the usual case: a `connect { authenticate }`
/// bound it) or, when the subscription supplied a per-request `auth` selection
/// (§11.4) rather than naming a stored context, from that retained selection —
/// so a subscription opened with an inline credential keeps re-verifying it as
/// state advances, catching revocation and expiry exactly as a stored context
/// does (§11.7).
#[derive(Debug, Clone)]
pub struct WatchAuthz {
    context: Option<String>,
    role: Option<String>,
    selection: Option<AuthSelection>,
}

impl WatchAuthz {
    /// A public subscription: no context, no role re-check.
    #[must_use]
    pub fn public() -> Self {
        Self { context: None, role: None, selection: None }
    }

    /// A role subscription bound to authentication `context` and gated by `role`,
    /// re-authorized from the connection's stored context at each frontier.
    #[must_use]
    pub fn role(context: impl Into<String>, role: impl Into<String>) -> Self {
        Self { context: Some(context.into()), role: Some(role.into()), selection: None }
    }

    /// Retain the per-request `auth` selection this subscription opened under
    /// (§11.4), so its frontiers re-authorize from the credential itself rather
    /// than a connection-stored context.
    #[must_use]
    pub fn with_selection(mut self, selection: AuthSelection) -> Self {
        self.selection = Some(selection);
        self
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

    /// The retained per-request `auth` selection, if the subscription opened with
    /// one instead of a connection-stored context (§11.4).
    #[must_use]
    pub fn selection(&self) -> Option<&AuthSelection> {
        self.selection.as_ref()
    }
}

/// A live subscription's tracked state.
///
/// `last` retains the full authorized view for delta continuity and for the
/// window's neighbor tracking; `windowed`, present only for a bounded
/// subscription (§12.2), is the client-visible slice of that view.
pub struct Watch {
    view: String,
    authz: WatchAuthz,
    /// The surface `$params` arguments bound at open (§10.1), re-supplied on every
    /// re-evaluation so a parameterized `$view` sees the same arguments after each
    /// commit and time advance (§12.2, §14.1).
    args: BTreeMap<String, Value>,
    frontier: CommitSeq,
    last: Option<ViewResult>,
    window: Option<Window>,
    windowed: Option<Vec<ViewRow>>,
    closed: Option<String>,
}

impl Watch {
    /// Open a subscription over the runtime view `view`, at initial `frontier`,
    /// under `authz`, before any result has been delivered.
    #[must_use]
    pub fn open(view: impl Into<String>, authz: WatchAuthz, frontier: CommitSeq) -> Self {
        Self {
            view: view.into(),
            authz,
            args: BTreeMap::new(),
            frontier,
            last: None,
            window: None,
            windowed: None,
            closed: None,
        }
    }

    /// Open a bounded-window subscription (§12.2): the same view under a client
    /// window that keeps only a bounded slice incremental.
    #[must_use]
    pub fn windowed(
        view: impl Into<String>,
        authz: WatchAuthz,
        frontier: CommitSeq,
        window: Window,
    ) -> Self {
        Self {
            view: view.into(),
            authz,
            args: BTreeMap::new(),
            frontier,
            last: None,
            window: Some(window),
            windowed: None,
            closed: None,
        }
    }

    /// Bind the surface `$params` arguments this subscription re-supplies on every
    /// re-evaluation (§10.1).
    #[must_use]
    pub fn with_args(mut self, args: BTreeMap<String, Value>) -> Self {
        self.args = args;
        self
    }

    /// The runtime view this subscription reads.
    #[must_use]
    pub fn view(&self) -> &str {
        &self.view
    }

    /// The surface `$params` arguments bound for this subscription (§10.1).
    #[must_use]
    pub fn args(&self) -> &BTreeMap<String, Value> {
        &self.args
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
    /// This is the full authorized view; for a bounded subscription the
    /// client-visible slice is [`Watch::window_rows`].
    #[must_use]
    pub fn current(&self) -> Option<&ViewResult> {
        self.last.as_ref()
    }

    /// The client-visible windowed rows, for a bounded subscription (§12.2).
    #[must_use]
    pub fn window_rows(&self) -> Option<&[ViewRow]> {
        self.windowed.as_deref()
    }

    /// Whether the subscription has been closed, and why.
    #[must_use]
    pub fn close_reason(&self) -> Option<&str> {
        self.closed.as_deref()
    }

    /// Deliver the initial complete result at `frontier`, returning the `init`
    /// delta (§12.2). Called once, when the subscription opens. A bounded
    /// subscription also opens its window over `result`, which fails when a
    /// concrete anchor identifies no current occurrence.
    ///
    /// # Errors
    /// [`WindowError`] when a bounded subscription's anchor is absent at open.
    pub fn init(&mut self, result: ViewResult, frontier: CommitSeq) -> Result<ViewDelta, WindowError> {
        let delta = ViewDelta::between(None, &result);
        if let Some(window) = &mut self.window {
            self.windowed = Some(window.open(&result)?);
        }
        self.last = Some(result);
        self.frontier = frontier;
        Ok(delta)
    }

    /// Advance to `result` at `frontier`, returning the coherent patch delta from
    /// the prior result (§12.2). The applied client result equals `result` — the
    /// recomputed authorized view — by construction; a bounded subscription
    /// re-slices its window over the recomputed view, tracking its anchor across
    /// gaps and reappearances.
    pub fn advance(&mut self, result: ViewResult, frontier: CommitSeq) -> ViewDelta {
        let delta = ViewDelta::between(self.last.as_ref(), &result);
        if let Some(window) = &mut self.window {
            self.windowed = Some(window.refresh(&result));
        }
        self.last = Some(result);
        self.frontier = frontier;
        delta
    }

    /// Close the subscription at its current frontier with `reason` (§12.2). The
    /// cached result is released; no further deltas are delivered.
    pub fn close(&mut self, reason: impl Into<String>) {
        self.last = None;
        self.windowed = None;
        self.closed = Some(reason.into());
    }
}
