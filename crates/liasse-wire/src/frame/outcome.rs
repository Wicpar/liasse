//! The spec outcome of a `call`, `fetch`, or `operation` request (§8.9, §10, §11,
//! §12.3) — the response body, distinct from a transport
//! [`Fault`](crate::FaultCode).
//!
//! The connect layer maps the engine's `SurfaceOutcome` (and, for a status query,
//! `OperationStatus`) onto these variants: `committed`/`unchanged` are the two
//! success completions, `rejected` an admission refusal (message verbatim from the
//! runtime), `denied` an authorization refusal (message sanitized), `failed` a
//! window that could not open, and `unknown` an operation with no retained record.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::token::Ft;

/// The outcome of a request. Tagged by `status`. Frontier and commit positions are
/// opaque [`Ft`] tokens — a raw `CommitSeq` never reaches the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    /// The transition committed (§12.3). `frontier` is the connection frontier
    /// covering the commit (at least `commit`), and the response value, if any, was
    /// evaluated there.
    Committed {
        /// The connection frontier the outcome is reported at.
        frontier: Ft,
        /// The position the transition committed at.
        commit: Ft,
        /// The call's response value, if it produced one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<Value>,
    },
    /// The request changed nothing (§8.9); it was evaluated at `frontier`, which did
    /// not advance (§12.3: `unchanged` proves evaluation at the returned frontier).
    Unchanged {
        /// The frontier the request was evaluated at.
        frontier: Ft,
        /// The response value, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<Value>,
    },
    /// Admission refused an otherwise well-addressed, authorized request (§8.8): a
    /// failed check, a duplicate key, a dangling ref, the §12.3 identifier conflict,
    /// and so on. The message is the runtime's, verbatim.
    Rejected {
        /// A stable admission-refusal code.
        code: Code,
        /// The runtime's diagnostic, verbatim.
        message: String,
    },
    /// Authentication, roles, or exposure refused the request before admission
    /// (§10, §11). The message is sanitized — it never carries a credential or an
    /// internal detail.
    Denied {
        /// A stable authorization-refusal code.
        code: Code,
        /// A sanitized diagnostic.
        message: String,
    },
    /// A bounded window could not open (§12.2): its anchor named no occurrence, or
    /// the view is a scalar/aggregate that has no rows to bound.
    Failed {
        /// Which window-open precondition was broken.
        code: FailedCode,
    },
    /// No record is retained for an operation capability (§12.3) — it was never
    /// submitted, or its record expired by host policy.
    Unknown,
}

/// A stable refusal code carried by `rejected` and `denied`. It is an opaque
/// string minted server-side (mirroring the runtime's rejection/denial taxonomy)
/// so this crate stays decoupled from that vocabulary while still typing the field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Code(String);

impl Code {
    /// Wrap a stable code string.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The code as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Code {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for Code {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Which precondition of opening a bounded window was broken (§12.2), mirroring the
/// runtime's `WindowError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailedCode {
    /// The window's concrete anchor identified no current occurrence.
    AbsentAnchor,
    /// The window was requested over a scalar/aggregate view, which delivers a
    /// value, not rows.
    ScalarView,
}
