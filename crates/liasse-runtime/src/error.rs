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
    /// A declared `$keyring` whose `$provider` names an application-registered
    /// real provider could not be provisioned at load (§17.5/§17.6): the
    /// provider fails the policy capability check, or its first version cannot be
    /// generated/bound. A named provider that cannot fulfil its ring is a loud
    /// load failure — the engine NEVER silently downgrades it to the forgeable
    /// sim double (fail-before-activation).
    Keyring(String),
    /// The store reported a structural or durability error.
    Store(StoreError),
    /// Genesis seed admission was refused by the rule pipeline (§9.1/§9.2): the
    /// package does not activate.
    Seed(Rejection),
    /// An engine invariant was violated at run time — a bug or corrupt durable
    /// state, never reachable from a well-formed request.
    Internal(String),
    /// A well-formed request the current build cannot serve without silent data
    /// loss, so it is refused rather than completed partially (fail-closed). The
    /// standing case is an export (§19.5) of an instance holding committed rows in
    /// a nested keyed collection (§5.4) the portable state capture does not carry
    /// through: emitting the artifact anyway would drop that live data (§20.1 "the
    /// compatible value is copied", §22.1 committed-state integrity). Carrying
    /// nested collections faithfully is a tracked feature, not a bug.
    Unsupported(String),
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
            Self::Keyring(detail) => write!(f, "keyring provider failed to load: {detail}"),
            Self::Store(error) => write!(f, "store error: {error}"),
            Self::Seed(rejection) => write!(f, "seed rejected: {}", rejection.message()),
            Self::Internal(detail) => write!(f, "engine invariant violated: {detail}"),
            Self::Unsupported(detail) => write!(f, "operation refused to avoid data loss: {detail}"),
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
    /// A deletion was blocked by a live inbound `$on_delete: restrict` reference,
    /// or two `$on_delete` patches assigned a field conflictingly (§21.1).
    Restricted,
    /// A well-typed expression failed at run time during admission (a zero
    /// divisor, a scalar-row selector matching not-exactly-one row, …).
    Evaluation,
    /// The request itself was malformed — unknown mutation, argument that does
    /// not decode to its declared type, missing receiver.
    Malformed,
    /// A host-namespace call refused the operation (§16.3/§17.9): a verifier
    /// rejected its input, a bound provider or keyring version was unavailable,
    /// or the returned value did not conform to the pinned contract. No
    /// application effect is committed.
    Host,
    /// A package update narrows the exposed boundary contract within one major
    /// (Annex E, §20.3): a removed surface or operation, a removed or
    /// type-narrowed output member, a changed exhaustive enum result, an added
    /// required parameter, or a narrowed accepted input domain. `load` and update
    /// reject the narrowing release before activation (E.1, E.9).
    Compatibility,
    /// A migration would need a §20 state carry the current build does not
    /// implement, so it is refused rather than committed with silent data loss
    /// (fail-closed). The standing case is an instance holding committed rows in a
    /// nested keyed collection (§5.4): the CORE portable capture carries top-level
    /// collections and the §8.2 singleton only, so a migration cannot copy those
    /// nested rows forward. Refusing keeps §20.1 ("the compatible value is copied")
    /// and §22.1 (committed-state integrity) rather than dropping live rows and
    /// reporting `committed`. Faithful nested-collection carry-through is a tracked
    /// feature.
    Unsupported,
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
        // §16.3: a host-namespace call refusal in an expression position (a
        // nonconforming return, a verifier rejection, an unavailable dependency)
        // is a host refusal, not an ordinary evaluation fault.
        let reason = match &error {
            EvalError::HostCall { .. } | EvalError::NoHostDispatch => RejectionReason::Host,
            _ => RejectionReason::Evaluation,
        };
        Self::new(reason, error.message())
    }
}
