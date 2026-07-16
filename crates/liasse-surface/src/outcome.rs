//! The typed result of an external surface operation (§10, §11, §12).
//!
//! The corpus separates three failure classes (`tests/FORMAT.md`, and the §10/§11
//! `NOTES.md`):
//!
//! - **`denied`** — refused by authentication, roles, or exposure: an
//!   unresolvable or ungranted name, an unaccepted authenticator, a failed
//!   `$verify`/`$check`, an invalid session, a non-member actor;
//! - **`rejected`** — an *admission* refusal of an otherwise well-addressed,
//!   authorized request (checks, keys, refs, uniqueness), surfaced verbatim from
//!   the runtime, plus the §12.3 burned-identifier conflict;
//! - **`committed` / `unchanged`** — the two success completions (§12.3, §8.9).
//!
//! Keeping [`Denial`] a distinct type from the runtime's [`Rejection`] is the
//! point: exposure and authorization are Liasse's permission mechanism, and a
//! permission failure must never be confused with an admission failure.

use liasse_runtime::{CommitSeq, Rejection, ResponseValue};

/// The success completion a call reported (§12.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// The transition committed and the connection frontier advanced through it.
    Committed,
    /// The request produced no state change (§8.9); the frontier did not move.
    Unchanged,
}

/// The outcome of an external surface operation.
#[derive(Debug, Clone)]
pub enum SurfaceOutcome {
    /// The call committed at `commit`; the connection frontier advanced to
    /// `frontier` (at least `commit`, §12.3) and its subscriptions were swept
    /// through it before the response returned. `frontier` is recorded at commit
    /// time so a retained §12.3 status reports the connection's actual frontier —
    /// which may be past `commit` when the connection already led it — rather than
    /// re-deriving it from `commit`.
    Committed { frontier: CommitSeq, commit: CommitSeq, response: Option<ResponseValue> },
    /// The call changed nothing (§8.9); the response was evaluated at `frontier`
    /// (§12.3: "Receiving `unchanged` proves evaluation at the returned
    /// frontier"), and the frontier did not advance.
    Unchanged { frontier: CommitSeq, response: Option<ResponseValue> },
    /// The runtime refused admission, or the §12.3 identifier conflict fired.
    Rejected(Rejection),
    /// Authentication, roles, or exposure refused the request before admission.
    Denied(Denial),
}

impl SurfaceOutcome {
    /// The success completion, if the operation succeeded.
    #[must_use]
    pub fn completion(&self) -> Option<Completion> {
        match self {
            Self::Committed { .. } => Some(Completion::Committed),
            Self::Unchanged { .. } => Some(Completion::Unchanged),
            _ => None,
        }
    }

    /// Whether the operation succeeded (committed or unchanged).
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Committed { .. } | Self::Unchanged { .. })
    }

    /// The commit position, if the call committed.
    #[must_use]
    pub fn commit(&self) -> Option<CommitSeq> {
        match self {
            Self::Committed { commit, .. } => Some(*commit),
            _ => None,
        }
    }

    /// The frontier a successful outcome was returned at: the connection frontier
    /// covering the commit for a committed call (at least `commit`, §12.3), the
    /// evaluation position for an unchanged one.
    #[must_use]
    pub fn frontier(&self) -> Option<CommitSeq> {
        match self {
            Self::Committed { frontier, .. } | Self::Unchanged { frontier, .. } => Some(*frontier),
            _ => None,
        }
    }

    /// The response value delivered with a successful outcome, if any.
    #[must_use]
    pub fn response(&self) -> Option<&ResponseValue> {
        match self {
            Self::Committed { response, .. } | Self::Unchanged { response, .. } => response.as_ref(),
            _ => None,
        }
    }

    /// The denial, if the operation was refused by the surface layer.
    #[must_use]
    pub fn denial(&self) -> Option<&Denial> {
        match self {
            Self::Denied(denial) => Some(denial),
            _ => None,
        }
    }

    /// The runtime rejection, if admission refused the request.
    #[must_use]
    pub fn rejection(&self) -> Option<&Rejection> {
        match self {
            Self::Rejected(rejection) => Some(rejection),
            _ => None,
        }
    }
}

/// Why the surface layer refused a request before (or instead of) admission —
/// the `denied` outcome class. The corpus asserts only the class, never the
/// finer reason (§10/§11 `NOTES.md`), so this enum documents the taxonomy the
/// spec leaves open (SPEC-ISSUES item 8: `denied` vs `not-found` is unpinned,
/// and a nonexistent surface need not be distinguishable from an ungranted one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialReason {
    /// The address named no surface/call exposed to the caller — a nonexistent
    /// surface, an undeclared call, or an internal declaration (§10.1, §12.1).
    Unresolved,
    /// A role surface was addressed with no authenticated actor (§10.2, §11).
    Unauthenticated,
    /// The targeted role does not accept the named authenticator (§11.4).
    AuthenticatorNotAccepted,
    /// The request named no authenticator where one is required (§11.4).
    AuthenticatorMissing,
    /// `$verify` rejected the credential — forged, tampered, or malformed
    /// (§11.3).
    Unverified,
    /// A verified proof did not bind to the selected authenticator, or an
    /// authenticator `$check` failed (§11.3, §11.4).
    CheckFailed,
    /// `$session` resolved zero or several rows, or the session is revoked or
    /// expired (§11.3, §11.7).
    SessionInvalid,
    /// `$actor` resolved zero or several rows (§11.3).
    ActorUnresolved,
    /// The resolved actor is not a member of the targeted role (§10.3).
    NotAMember,
}

/// A surface-layer refusal: its reason and a human-readable diagnostic. It never
/// carries application state, only the class and a message (§11.3: audit records
/// hold names and stable codes, not credentials).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    reason: DenialReason,
    message: String,
}

impl Denial {
    /// Build a denial of `reason` with a diagnostic `message`.
    #[must_use]
    pub fn new(reason: DenialReason, message: impl Into<String>) -> Self {
        Self { reason, message: message.into() }
    }

    /// The refusal class.
    #[must_use]
    pub fn reason(&self) -> DenialReason {
        self.reason
    }

    /// The diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}
