//! Engine-level failures and application rejections.
//!
//! Two failure kinds are kept apart on purpose. An [`EngineError`] is a
//! host-facing problem with the definition, the host requirements, or the store
//! — it is not something an application request can trigger. A [`Rejection`] is
//! an *admission* refusal (§8.8, §5, §22.2): a well-formed request that the rule
//! pipeline declined, leaving the prior committed state intact.

use liasse_diag::Diagnostics;
use liasse_expr::EvalError;
use liasse_store::StoreError;

/// A failure to load, replay, or operate the engine — distinct from an
/// application-level [`Rejection`], which the store never sees.
#[derive(Debug)]
pub enum EngineError {
    /// The definition text did not parse or did not pass static validation
    /// (§9.2 step 5). Carries the accumulated diagnostics.
    Invalid(Box<Diagnostics>),
    /// A required host namespace, provider, or connector did not resolve
    /// against the registry (§9.2 step 4, fail-before-activation).
    Requirement(String),
    /// The store reported a structural or durability error.
    Store(StoreError),
    /// Genesis seed admission was refused by the rule pipeline (§9.1/§9.2): the
    /// package does not activate.
    Seed(Rejection),
    /// An engine invariant was violated at run time — a bug or corrupt durable
    /// state, never reachable from a well-formed request.
    Internal(String),
}

impl From<StoreError> for EngineError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl core::fmt::Display for EngineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Invalid(_) => f.write_str("the definition failed static validation"),
            Self::Requirement(name) => write!(f, "unresolved host requirement: {name}"),
            Self::Store(error) => write!(f, "store error: {error}"),
            Self::Seed(rejection) => write!(f, "seed rejected: {}", rejection.message()),
            Self::Internal(detail) => write!(f, "engine invariant violated: {detail}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// The class of rule that refused a request. Mirrors the corpus rejection
/// vocabulary the admission pipeline enforces (§5, §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    /// A `$mut` `assert(...)` condition evaluated false (§8.8).
    Assertion,
    /// A field, struct, or row `$check` failed over the prospective state
    /// (§5.10, §8.8).
    Check,
    /// A collection key already names a live row in its parent (§5.4).
    DuplicateKey,
    /// A `$ref` did not resolve to a live target occurrence (§5.6).
    DanglingRef,
    /// A `$unique` candidate key collided with another row (§5.7).
    Uniqueness,
    /// An assigned or inserted value was not of the field's declared type
    /// (§8.5).
    TypeError,
    /// A keyed row patch, delete, or field write named an absent row (§8.9).
    MissingTarget,
    /// A well-typed expression failed at run time during admission (a zero
    /// divisor, a scalar-row selector matching not-exactly-one row, …).
    Evaluation,
    /// The request itself was malformed — unknown mutation, argument that does
    /// not decode to its declared type, missing receiver.
    Malformed,
}

/// An admission refusal: the class, a human message, and the state path it
/// concerns when one is known (§8.8 structured diagnostic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    reason: RejectionReason,
    message: String,
    path: Option<String>,
}

impl Rejection {
    /// Build a rejection of `reason` with a diagnostic `message`.
    #[must_use]
    pub fn new(reason: RejectionReason, message: impl Into<String>) -> Self {
        Self { reason, message: message.into(), path: None }
    }

    /// Attach the state path this rejection concerns.
    #[must_use]
    pub fn at(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// The rule class that refused the request.
    #[must_use]
    pub fn reason(&self) -> RejectionReason {
        self.reason
    }

    /// The human-readable diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The state path this rejection concerns, if known.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }
}

impl From<EvalError> for Rejection {
    fn from(error: EvalError) -> Self {
        Self::new(RejectionReason::Evaluation, error.message())
    }
}
