//! A logical client connection (SPEC.md §12): the unit that owns live
//! subscriptions and carries the completion barrier.
//!
//! A connection records a frontier — the exact commit progress reflected on it —
//! and owns the watches opened over it. A successful call advances that frontier
//! to at least the returned commit and drags every still-authorized subscription
//! through it (§12.3, §12.6). A single connection MAY multiplex several
//! authentication contexts (§11.8); each subscription and call selects one, while
//! the connection-level barrier advances all of them.

use std::collections::BTreeMap;

use liasse_runtime::CommitSeq;

use crate::request::AuthSelection;
use crate::watch::Watch;

/// The name of a connection's implicit authentication context — the one opened
/// by `connect { authenticate }` and used when a request names no other (§11.8).
pub const DEFAULT_CONTEXT: &str = "default";

/// One authentication context bound on a connection: the client-supplied
/// *selection* (authenticator name + credential) together with the role it
/// authenticated against.
///
/// The role scopes §10.4's established-authority exception to the exact role
/// this context proved authority over. A context bound against role `alpha` is
/// established authority over `alpha` alone — never over an unrelated role
/// `beta` the caller never authenticated to. Without the role a bound context
/// would read as established authority over *every* role, so a denial over
/// `beta` (which does not accept `alpha`'s authenticator) would leak `beta`'s
/// precise reason while a nonexistent role denies the uniform `unresolved` —
/// exactly the role-existence enumeration oracle §10.4 forbids.
struct BoundContext {
    /// The role this context authenticated against (§11.4, `authenticate`).
    role: String,
    /// The selection the client supplied, re-verified at every request (§11.4).
    selection: AuthSelection,
}

/// One logical client connection.
///
/// A connection retains each authentication context as the *selection* the
/// client supplied — the authenticator name and credential — not a resolved
/// actor. §11.4 makes verification explicit at every external request, so the
/// selection is re-verified against committed state on each call and at each
/// outgoing subscription frontier; a revoked or expired session therefore denies
/// the very next request rather than lingering as a stale grant (§11.7). The
/// credential is retained only in transport state, never written to application
/// state (§11.3). Each context also retains the role it authenticated against so
/// a denial can tell whether the caller has established authority over a *given*
/// target role (§10.4).
pub struct Connection {
    frontier: CommitSeq,
    contexts: BTreeMap<String, BoundContext>,
    watches: BTreeMap<String, Watch>,
}

impl Connection {
    /// Open a connection whose frontier starts at `frontier` (the head at
    /// connect time) with no authentication contexts and no subscriptions.
    #[must_use]
    pub fn new(frontier: CommitSeq) -> Self {
        Self { frontier, contexts: BTreeMap::new(), watches: BTreeMap::new() }
    }

    /// The connection's current frontier.
    #[must_use]
    pub fn frontier(&self) -> CommitSeq {
        self.frontier
    }

    /// Advance the frontier to at least `seq` (§12.3). Monotone: a stale or equal
    /// position never moves it backward.
    pub fn advance_frontier(&mut self, seq: CommitSeq) {
        if seq > self.frontier {
            self.frontier = seq;
        }
    }

    /// Bind authentication selection `name`, established against `role`, on this
    /// connection (§11.8). Retaining `role` scopes §10.4's established-authority
    /// exception to the exact role this context proved authority over, so a later
    /// denial over any *other* role stays hidden as the uniform `unresolved`.
    pub fn set_context(
        &mut self,
        name: impl Into<String>,
        role: impl Into<String>,
        selection: AuthSelection,
    ) {
        self.contexts.insert(name.into(), BoundContext { role: role.into(), selection });
    }

    /// The authentication selection named `name`, if bound.
    #[must_use]
    pub fn context(&self, name: &str) -> Option<&AuthSelection> {
        self.contexts.get(name).map(|bound| &bound.selection)
    }

    /// Resolve the selection a request uses: the named one, or the default when
    /// the request names none.
    #[must_use]
    pub fn select_context(&self, name: Option<&str>) -> Option<&AuthSelection> {
        self.contexts.get(name.unwrap_or(DEFAULT_CONTEXT)).map(|bound| &bound.selection)
    }

    /// The role the selected context established authority over (§10.4), or
    /// `None` when no such context is bound. A request authorizing from this
    /// context has established authority over the returned role and no other, so
    /// the caller reads a role's precise (membership-/existence-specific)
    /// diagnostics only when it is *this* role — every other role's denial is
    /// hidden as the uniform unresolvable-name outcome.
    #[must_use]
    pub fn context_role(&self, name: Option<&str>) -> Option<&str> {
        self.contexts.get(name.unwrap_or(DEFAULT_CONTEXT)).map(|bound| bound.role.as_str())
    }

    /// The bound context names, for the manifest (§12.1).
    pub fn context_names(&self) -> impl Iterator<Item = &String> {
        self.contexts.keys()
    }

    /// Open subscription `id` over this connection.
    pub fn insert_watch(&mut self, id: impl Into<String>, watch: Watch) {
        self.watches.insert(id.into(), watch);
    }

    /// The subscription named `id`, if open.
    #[must_use]
    pub fn watch(&self, id: &str) -> Option<&Watch> {
        self.watches.get(id)
    }

    /// The subscription named `id` for mutation, if open.
    pub fn watch_mut(&mut self, id: &str) -> Option<&mut Watch> {
        self.watches.get_mut(id)
    }

    /// Remove subscription `id`, returning it if it was open.
    pub fn remove_watch(&mut self, id: &str) -> Option<Watch> {
        self.watches.remove(id)
    }

    /// Every open subscription's id, for the connection-wide barrier sweep.
    #[must_use]
    pub fn watch_ids(&self) -> Vec<String> {
        self.watches.keys().cloned().collect()
    }
}
