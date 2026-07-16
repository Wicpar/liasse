//! The FORMAT.md outcome vocabulary and its sibling completion/status enums.
//!
//! These are closed sets: an outcome is exactly one of six tokens, and the
//! parser rejects anything else so an invalid outcome is unrepresentable past
//! the boundary.

use std::fmt;

/// The result an `expect` block asserts, per the FORMAT.md outcome table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// Accepted / succeeds.
    Ok,
    /// Statically rejected at build/load/validation time.
    Invalid,
    /// Rejected by authentication, roles, or permissions.
    Denied,
    /// Admission-time rejection (checks, keys, refs, uniqueness, meters, limits).
    Rejected,
    /// Other runtime failure the spec mandates.
    Error,
    /// The spec does not pin the behavior; carries no `violates`.
    Unspecified,
}

impl Outcome {
    /// Parse one bare outcome token.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "ok" => Self::Ok,
            "invalid" => Self::Invalid,
            "denied" => Self::Denied,
            "rejected" => Self::Rejected,
            "error" => Self::Error,
            "unspecified" => Self::Unspecified,
            _ => return None,
        })
    }

    /// The canonical bare token.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Invalid => "invalid",
            Self::Denied => "denied",
            Self::Rejected => "rejected",
            Self::Error => "error",
            Self::Unspecified => "unspecified",
        }
    }

    /// Whether FORMAT.md requires a `violates` array for this outcome.
    ///
    /// Every non-`ok` outcome names the violated rule except `unspecified`,
    /// which asserts that no rule pins the behavior.
    #[must_use]
    pub fn requires_violates(self) -> bool {
        matches!(self, Self::Invalid | Self::Denied | Self::Rejected | Self::Error)
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

/// The success completion an `outcome: ok` call reported (§8.9, §12.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Completion {
    /// A commit was created and is final.
    Committed,
    /// No state change; no commit; the client frontier does not advance.
    Unchanged,
}

impl Completion {
    /// Parse one bare completion token.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "committed" => Self::Committed,
            "unchanged" => Self::Unchanged,
            _ => return None,
        })
    }

    /// The canonical bare token.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Unchanged => "unchanged",
        }
    }
}

impl fmt::Display for Completion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

/// The status a queried operation record reports (§12.3 `operation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationStatus {
    /// Admitted but not yet resolved.
    Pending,
    /// Completed with a commit.
    Committed,
    /// Completed with no state change.
    Unchanged,
    /// Refused at admission.
    Rejected,
    /// No record is retained for the identifier.
    Unknown,
}

impl OperationStatus {
    /// Parse one bare status token.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "pending" => Self::Pending,
            "committed" => Self::Committed,
            "unchanged" => Self::Unchanged,
            "rejected" => Self::Rejected,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    /// The canonical bare token.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Committed => "committed",
            Self::Unchanged => "unchanged",
            Self::Rejected => "rejected",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for OperationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}
