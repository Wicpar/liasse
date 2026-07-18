//! A live subscription over a surface view (SPEC.md §12.2).
//!
//! A subscription begins with a complete result and a frontier, then receives
//! ordered patches; after applying every patch the client result MUST equal the
//! authorized declared view at the new frontier (§12.2). This layer recomputes the
//! view at each frontier through the engine's [`ViewResult`] and emits the §12.2
//! [`ViewDelta`] between the client's prior result and the new one. For a
//! row-stream view that is the ordered `insert`/`remove`/`move`/`update` sequence
//! carrying the client's prior ordered result to the recomputed view, order
//! included; for a scalar/aggregate view (§7.5) it is the value form — the new
//! value when it changed, a frontier-only no-op when it did not.
//!
//! A bounded subscription's *client result is its window* (§12.2), so its delta is
//! diffed over the window slices — the prior client-visible window against the
//! refreshed one — not the full view: positions are window-relative and a row the
//! window's shift evicts renders as a `remove`, so applying the delta to the
//! client's prior window reproduces the new authorized window exactly.
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
/// `last` retains the full authorized view for the window's neighbor tracking (and
/// as an unwindowed subscription's delta-continuity prior); `windowed`, present
/// only for a bounded subscription (§12.2), is the client-visible slice of that
/// view and the prior slice each windowed delta is diffed against.
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
    /// delta (§12.2). For an unwindowed subscription that is the full view: a
    /// row-stream view's complete rows, or a scalar/aggregate view's value (§7.5).
    /// For a bounded subscription the client result is its WINDOW, so the `init`
    /// ships the window's rows — opening the window over `result` first, which fails
    /// when a concrete anchor identifies no current occurrence. Called once, when
    /// the subscription opens.
    ///
    /// # Errors
    /// [`WindowError`] when a bounded subscription's anchor is absent at open.
    pub fn init(&mut self, result: ViewResult, frontier: CommitSeq) -> Result<ViewDelta, WindowError> {
        let delta = if let Some(window) = &mut self.window {
            // §12.2: a bounded subscription's client result is its window, so its
            // init ships the window's rows and its later deltas diff against the
            // window (see `advance`) — never the full view.
            let rows = window.open(&result)?;
            let delta = ViewDelta::between_rows(None, &rows);
            self.windowed = Some(rows);
            delta
        } else {
            ViewDelta::between(None, &result)
        };
        self.last = Some(result);
        self.frontier = frontier;
        Ok(delta)
    }

    /// Advance to `result` at `frontier`, returning the coherent delta from the
    /// client's prior result (§12.2). After applying it the client result equals
    /// the authorized declared view at the new frontier — and *the client result is
    /// what the client tracks*:
    ///
    /// - an unwindowed subscription tracks the full view, so the delta diffs the
    ///   full prior result against `result`: a row-stream view's ordered patch, or a
    ///   scalar view's new value (frontier-only when unchanged, §7.5);
    /// - a bounded subscription tracks its WINDOW, so it re-slices the window over
    ///   the recomputed view (following its anchor across gaps and reappearances)
    ///   and the delta diffs the *prior window slice* against the *refreshed window
    ///   slice*. Positions are window-relative, and a row the window's shift pushed
    ///   past its `$size` bound renders as a `remove` — so applying the delta to the
    ///   client's prior window reproduces the new authorized window exactly, never
    ///   the whole view.
    pub fn advance(&mut self, result: ViewResult, frontier: CommitSeq) -> ViewDelta {
        let delta = if let Some(window) = &mut self.window {
            // §12.2: diff the client's own prior window against the refreshed one,
            // so evictions become removes and positions stay inside the window.
            let refreshed = window.refresh(&result);
            let delta = ViewDelta::between_rows(self.windowed.as_deref(), &refreshed);
            self.windowed = Some(refreshed);
            delta
        } else {
            ViewDelta::between(self.last.as_ref(), &result)
        };
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
