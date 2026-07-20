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
/// authenticated against and whether it demonstrated authority (verified
/// membership) over that role at `authenticate`.
///
/// The role scopes §10.4's established-authority exception to the exact role
/// this context proved authority over. A context bound against role `alpha` is
/// established authority over `alpha` alone — never over an unrelated role
/// `beta` the caller never authenticated to. Without the role a bound context
/// would read as established authority over *every* role, so a denial over
/// `beta` (which does not accept `alpha`'s authenticator) would leak `beta`'s
/// precise reason while a nonexistent role denies the uniform `unresolved` —
/// exactly the role-existence enumeration oracle §10.4 forbids.
///
/// §10.4's exception is for a caller that "has already established authority
/// over the target" — a *member*. `authenticate` binds a context whenever the
/// selection merely verifies (the role accepts the authenticator and the
/// credential resolves an actor/session), *without* checking membership
/// (§11.8), so the role alone does not prove authority: a non-member whose
/// session resolves would be recorded against the role and later read as
/// established, leaking the role's existence through the authentication-FAILURE
/// path (e.g. a `session-invalid` after revocation). `member` records whether
/// the actor was a verified member of `role` at `authenticate`, so establishment
/// requires DEMONSTRATED authority, not merely having named the role.
struct BoundContext {
    /// The role this context authenticated against (§11.4, `authenticate`).
    role: String,
    /// Whether the actor was a verified member of `role` at `authenticate`
    /// (§10.4) — the demonstrated authority §10.4's exception requires. A member
    /// whose session later expires or is revoked stays `member = true`, so it
    /// still reads its OWN role's `session-invalid` (§11.7); a non-member is
    /// `member = false`, so every role denial collapses to the uniform
    /// `unresolved`.
    member: bool,
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

    /// Bind authentication selection `name`, authenticated against `role`, on
    /// this connection (§11.8), recording whether the actor was a verified
    /// `member` of that role at `authenticate`. Retaining `role` scopes §10.4's
    /// established-authority exception to the exact role this context
    /// authenticated against, so a later denial over any *other* role stays
    /// hidden as the uniform `unresolved`; gating on `member` further requires
    /// the caller to have DEMONSTRATED authority over `role`, so a non-member
    /// whose credential merely resolves never reads that role's precise denials.
    pub fn set_context(
        &mut self,
        name: impl Into<String>,
        role: impl Into<String>,
        member: bool,
        selection: AuthSelection,
    ) {
        self.contexts.insert(name.into(), BoundContext { role: role.into(), member, selection });
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

    /// Whether the selected context has established authority over `role`
    /// (§10.4): it authenticated against `role` AND was a verified member of it
    /// at `authenticate`. Only then does the caller read that role's precise
    /// (membership-/existence-specific) diagnostics; a context bound against a
    /// *different* role, or against `role` as a non-member, has NOT established
    /// authority over `role`, so its denial is hidden as the uniform
    /// unresolvable-name outcome. `false` when no such context is bound.
    #[must_use]
    pub fn establishes(&self, name: Option<&str>, role: &str) -> bool {
        self.contexts
            .get(name.unwrap_or(DEFAULT_CONTEXT))
            .is_some_and(|bound| bound.member && bound.role == role)
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
