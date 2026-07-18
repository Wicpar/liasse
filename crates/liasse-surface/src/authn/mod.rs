//! Authenticators (SPEC.md §11): verify one credential, resolve an actor, and
//! optionally continue a session.
//!
//! The model validates authenticator *declarations* (`$credential`, `$verify`,
//! `$actor`, `$session`, `$check`) but leaves their execution a documented seam:
//! `$verify` binds `$proof` through a host verifier namespace whose typed
//! signature the model does not hold (`crates/liasse-model/src/auth.rs`). This
//! layer supplies that seam. A [`Verifier`] stands in for the `$verify`
//! namespace — it turns a credential into typed [`Claims`] — and an
//! [`Authenticator`] wires those claims to committed application rows through the
//! read-only [`StateReader`], enforcing the §11.3 rules: the proof binds to the
//! selected authenticator, `$session`/`$actor` resolve exactly one row, and the
//! session is active (§11.7).

mod identity;
mod session;

pub use identity::{Actor, AuthContext, RowLookup, RowSource, Session};
pub use session::{SessionAuthenticator, SessionSource};

use liasse_value::Value;

use crate::outcome::Denial;
use crate::reader::StateReader;

/// The credential a client supplies for one call (`$credential`, §11.3). It
/// lives for the call only; nothing here writes it to application state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential(Value);

impl Credential {
    /// Wrap a typed credential value.
    #[must_use]
    pub fn new(value: Value) -> Self {
        Self(value)
    }

    /// The underlying credential value, for a verifier to inspect.
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.0
    }
}

/// The typed result of verifying a credential (`$proof`, §11.3): the
/// authenticator the proof is bound to, and the keys selecting its session and
/// account rows. A conforming verifier binds `auth` cryptographically (audience,
/// issuer, signed claim) so a proof minted for one authenticator cannot be
/// replayed against another (§11.4, `red/cross-authenticator-proof-binding`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    auth: String,
    session: Option<Value>,
    account: Option<Value>,
}

impl Claims {
    /// Assemble a proof's claims.
    #[must_use]
    pub fn new(auth: impl Into<String>, session: Option<Value>, account: Option<Value>) -> Self {
        Self { auth: auth.into(), session, account }
    }

    /// The authenticator the proof is bound to.
    #[must_use]
    pub fn auth(&self) -> &str {
        &self.auth
    }

    /// The session key claimed, if any.
    #[must_use]
    pub fn session(&self) -> Option<&Value> {
        self.session.as_ref()
    }

    /// The account key claimed, if any.
    #[must_use]
    pub fn account(&self) -> Option<&Value> {
        self.account.as_ref()
    }
}

/// A credential the verifier could not turn into a proof — forged, tampered, or
/// malformed (§11.3). It never carries the credential bytes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct VerifyFailure {
    message: String,
}

impl VerifyFailure {
    /// A verification failure with diagnostic `message`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

/// The `$verify` host seam (§11.3, §16.3): turn a credential into a typed proof,
/// or fail. It performs no application-state mutation.
pub trait Verifier {
    /// Verify `credential`, returning its embedded claims or a failure.
    ///
    /// # Errors
    /// A credential this verifier did not issue (or that was tampered) fails.
    fn verify(&self, credential: &Credential) -> Result<Claims, VerifyFailure>;
}

/// An authenticator (§11.3): the runtime piece a targeted role selects to admit
/// a request. It owns the whole §11.3 resolution — verify, bind, resolve
/// session/actor, check — and yields a resolved [`AuthContext`] or a [`Denial`].
pub trait Authenticator {
    /// The name a role's `$auth` selection and a request name it by (§11.4).
    fn name(&self) -> &str;

    /// Resolve `credential` into an authentication context against committed
    /// state, or deny.
    ///
    /// # Errors
    /// Returns a [`Denial`] for every §11 refusal. A store/engine fault is
    /// surfaced as [`DenialReason::SessionInvalid`]/[`ActorUnresolved`] rather
    /// than propagated, so authentication never crashes a request.
    ///
    /// [`DenialReason::SessionInvalid`]: crate::DenialReason::SessionInvalid
    /// [`ActorUnresolved`]: crate::DenialReason::ActorUnresolved
    fn resolve(&self, credential: &Credential, reader: &dyn StateReader) -> Result<AuthContext, Denial>;
}
