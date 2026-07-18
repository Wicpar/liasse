//! Projecting the engine's results and outcomes onto the wire (§8.10, §12).
//!
//! Every value that leaves the server is the *already-authorized projection*: a row
//! renders as the object of its exposed fields (`Value::to_wire`, absent optionals
//! omitted), never its `RowId` or `$sort` tuple; a frontier or commit position is an
//! opaque [`Ft`] minted for this connection, never a raw `CommitSeq`; a response is
//! [`ResponseValue::to_wire`] (§8.10). Refusals split by class exactly as the
//! surface layer draws them — a `rejected` message is the runtime's verbatim, a
//! `denied` message is replaced by a stable sanitized one so no credential or
//! internal detail crosses the boundary (§11.3, AGENTS.md).

use liasse_runtime::RejectionReason;
use liasse_surface::{
    CommitSeq, Denial, DenialReason, OperationStatus, Rejection, SurfaceOutcome, ViewRow,
    WindowError,
};
use liasse_wire::serde_json::{Map, Value as Json};
use liasse_wire::{Code, FailedCode, Ft, Occ, Outcome, WireRow};

/// The exposed value of one view row: the object of its output fields in canonical
/// order, each rendered by the engine's canonical encoding. A `none` optional is
/// already absent from a [`ViewRow`]'s fields, so it never appears (Annex A).
#[must_use]
pub fn row_object(row: &ViewRow) -> Json {
    let mut map = Map::new();
    for (name, value) in row.fields() {
        map.insert(name.clone(), value.to_wire());
    }
    Json::Object(map)
}

/// A row on the wire: its per-subscription occurrence token and its exposed value.
#[must_use]
pub fn wire_row(row: &ViewRow, occ: Occ) -> WireRow {
    WireRow::new(occ, row_object(row))
}

/// Map a surface call/fetch outcome onto its wire [`Outcome`], minting frontier and
/// commit tokens through `ft`.
#[must_use]
pub fn outcome_of(outcome: &SurfaceOutcome, ft: impl Fn(CommitSeq) -> Ft) -> Outcome {
    match outcome {
        SurfaceOutcome::Committed { frontier, commit, response } => Outcome::Committed {
            frontier: ft(*frontier),
            commit: ft(*commit),
            response: response.as_ref().map(|r| r.to_wire()),
        },
        SurfaceOutcome::Unchanged { frontier, response } => Outcome::Unchanged {
            frontier: ft(*frontier),
            response: response.as_ref().map(|r| r.to_wire()),
        },
        SurfaceOutcome::Rejected(rejection) => rejected(rejection),
        SurfaceOutcome::Denied(denial) => denied(denial),
    }
}

/// Map a retained §12.3 operation status onto a wire [`Outcome`]. A status query has
/// no response payload to replay — only the completion class and its positions.
#[must_use]
pub fn status_outcome(status: &OperationStatus, ft: impl Fn(CommitSeq) -> Ft) -> Outcome {
    match status {
        OperationStatus::Committed { frontier, commit } => {
            Outcome::Committed { frontier: ft(*frontier), commit: ft(*commit), response: None }
        }
        OperationStatus::Unchanged { frontier } => {
            Outcome::Unchanged { frontier: ft(*frontier), response: None }
        }
        OperationStatus::Rejected => Outcome::Rejected {
            code: Code::new("rejected"),
            message: "the operation was refused at admission".to_owned(),
        },
        OperationStatus::Unknown => Outcome::Unknown,
    }
}

/// An admission refusal (§8.8): a stable code plus the runtime's diagnostic
/// verbatim. Admission messages carry no credential, so they pass through as-is.
#[must_use]
pub fn rejected(rejection: &Rejection) -> Outcome {
    Outcome::Rejected { code: rejection_code(rejection.reason()), message: rejection.message().to_owned() }
}

/// An authorization refusal (§10/§11): a stable code and a sanitized message. The
/// surface layer's own diagnostic is discarded — a `denied` message never carries a
/// credential or an internal identity (§11.3).
#[must_use]
pub fn denied(denial: &Denial) -> Outcome {
    let (code, message) = denial_code(denial.reason());
    Outcome::Denied { code: Code::new(code), message: message.to_owned() }
}

/// A window that could not open (§12.2): an absent concrete anchor, or a
/// scalar/aggregate view that has no rows to bound.
#[must_use]
pub fn window_failure(error: &WindowError) -> Outcome {
    let code = match error {
        WindowError::AbsentAnchor => FailedCode::AbsentAnchor,
        WindowError::ScalarView => FailedCode::ScalarView,
    };
    Outcome::Failed { code }
}

/// The absent-anchor failure, for a window whose anchor named an occurrence this
/// connection does not currently hold.
#[must_use]
pub fn absent_anchor() -> Outcome {
    Outcome::Failed { code: FailedCode::AbsentAnchor }
}

/// Map a decode refusal onto an outcome: a mistyped or unknown argument is a
/// `rejected` malformed request (mirroring the runtime), a bad credential a
/// sanitized `denied` (§11.3).
#[must_use]
pub fn decode_error(error: &crate::decode::DecodeError) -> Outcome {
    match error {
        crate::decode::DecodeError::Malformed(message) => {
            Outcome::Rejected { code: Code::new("malformed"), message: message.clone() }
        }
        crate::decode::DecodeError::Credential => unverified(),
    }
}

/// A synthetic unresolvable-address denial, for a name that does not even parse as a
/// surface address (§10.1) — indistinguishable from an ungranted one.
#[must_use]
pub fn unresolved() -> Outcome {
    Outcome::Denied {
        code: Code::new("unresolved"),
        message: "the address names nothing exposed to this caller".to_owned(),
    }
}

/// A synthetic credential-refused denial (§11.3).
#[must_use]
pub fn unverified() -> Outcome {
    Outcome::Denied {
        code: Code::new("unverified"),
        message: "the credential was not accepted".to_owned(),
    }
}

/// The stable admission-refusal code mirroring the runtime taxonomy (§8.8).
fn rejection_code(reason: RejectionReason) -> Code {
    let text = match reason {
        RejectionReason::Assertion => "assertion",
        RejectionReason::Check => "check",
        RejectionReason::DuplicateKey => "duplicate-key",
        RejectionReason::DanglingRef => "dangling-ref",
        RejectionReason::Uniqueness => "uniqueness",
        RejectionReason::TypeError => "type-error",
        RejectionReason::MissingTarget => "missing-target",
        RejectionReason::Restricted => "restricted",
        RejectionReason::Evaluation => "evaluation",
        RejectionReason::Malformed => "malformed",
        RejectionReason::Host => "host",
        RejectionReason::Compatibility => "compatibility",
        RejectionReason::Unsupported => "unsupported",
    };
    Code::new(text)
}

/// The stable authorization-refusal code and its sanitized message (§10/§11).
fn denial_code(reason: DenialReason) -> (&'static str, &'static str) {
    match reason {
        DenialReason::Unresolved => ("unresolved", "the address names nothing exposed to this caller"),
        DenialReason::Unauthenticated => {
            ("unauthenticated", "this surface requires an authenticated actor")
        }
        DenialReason::AuthenticatorNotAccepted => {
            ("authenticator-not-accepted", "the authenticator is not accepted here")
        }
        DenialReason::AuthenticatorMissing => {
            ("authenticator-missing", "an authenticator is required")
        }
        DenialReason::Unverified => ("unverified", "the credential was not accepted"),
        DenialReason::CheckFailed => ("check-failed", "the authenticator check failed"),
        DenialReason::SessionInvalid => ("session-invalid", "the session is not valid"),
        DenialReason::ActorUnresolved => ("actor-unresolved", "the actor could not be resolved"),
    }
}
