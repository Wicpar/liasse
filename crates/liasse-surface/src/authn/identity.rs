//! Resolved authentication identities and the row-lookup they resolve through.
//!
//! There is no separate actor registry (§10.3): an authenticator resolves one
//! ordinary application row as `$actor` and, when declared, one as `$session`.
//! These types are the resolved result, carried for the request's lifetime and
//! re-derived from committed state at every admission.

use liasse_runtime::{RefKey, Timestamp, Value, ViewResult, ViewRow};

use crate::reader::StateReader;

/// A value's *application key* (§5.6): a ref's application value is its target's
/// typed key, so a scalar-keyed ref dereferences to that key; every other value
/// is its own key. This lets a `$session.account` ref (`Value::Ref`) match the
/// accounts collection's scalar key when resolving `$actor` (§11.3).
fn application_key(value: &Value) -> &Value {
    match value {
        Value::Ref(reference) => match reference.key() {
            RefKey::Scalar(inner) => inner,
            RefKey::Composite(_) => value,
        },
        _ => value,
    }
}

/// The application row an authenticator selected as `$actor` (§11.3). Identity is
/// the row's key value; role membership tests compare exactly this identity
/// (§10.3 "its exact row identity occurs at least once").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    key: Value,
}

impl Actor {
    /// The actor resolved at `key`.
    #[must_use]
    pub fn new(key: Value) -> Self {
        Self { key }
    }

    /// The actor's row-key identity.
    #[must_use]
    pub fn key(&self) -> &Value {
        &self.key
    }
}

/// The application row an authenticator selected as `$session` (§11.2), with the
/// two lifetime fields the surface layer judges without model support: its
/// expiry instant and its revocation flag (§11.7).
///
/// The expiry is the session bucket's upper bound `$until` (§14.1). An absent
/// bound (`None`) leaves the interval unbounded (§14): the session is perpetual,
/// active until revoked (§11.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    key: Value,
    expires_at: Option<Timestamp>,
    revoked: bool,
}

impl Session {
    /// A session at `key` whose bucket upper bound is `expires_at` — `Some`
    /// instant for a finite lifetime, `None` for a perpetual session (§14
    /// unbounded upper bound) — revoked or not.
    #[must_use]
    pub fn new(key: Value, expires_at: Option<Timestamp>, revoked: bool) -> Self {
        Self { key, expires_at, revoked }
    }

    /// The session's row-key identity.
    #[must_use]
    pub fn key(&self) -> &Value {
        &self.key
    }

    /// The instant at and after which the session is no longer active, or `None`
    /// when the session never expires (§11.7, §14 unbounded upper bound).
    #[must_use]
    pub fn expires_at(&self) -> Option<Timestamp> {
        self.expires_at
    }

    /// Whether the application has revoked the session (§11.7).
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        self.revoked
    }

    /// Whether the session is active at `now`: not revoked and within its bucket
    /// interval. Expiry is half-open — a finite session is live strictly before
    /// `expires_at` and dead at it (§11.7, `red/session-expiry-half-open-boundary`);
    /// a session with no expiry is unbounded above, so it stays active until
    /// revoked (§14 omitted upper bound).
    #[must_use]
    pub fn is_active_at(&self, now: Timestamp) -> bool {
        !self.revoked && self.expires_at.is_none_or(|until| now < until)
    }
}

/// A fully resolved authentication context (§11.1): the authenticator that
/// admitted it, the actor, and the optional session. One logical connection MAY
/// hold several of these at once (§11.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    auth_name: String,
    actor: Actor,
    session: Option<Session>,
}

impl AuthContext {
    /// Assemble a resolved context.
    #[must_use]
    pub fn new(auth_name: impl Into<String>, actor: Actor, session: Option<Session>) -> Self {
        Self { auth_name: auth_name.into(), actor, session }
    }

    /// The authenticator that produced this context.
    #[must_use]
    pub fn auth_name(&self) -> &str {
        &self.auth_name
    }

    /// The resolved actor.
    #[must_use]
    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    /// The resolved session, when the authenticator declared one.
    #[must_use]
    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }
}

/// Names one application view and the field within it that holds a row's lookup
/// key — the surface layer's stand-in for a `/collection[$key]` selection (§11.3)
/// evaluated through the engine's named-view API. `$session`/`$actor` MUST each
/// resolve exactly one row; zero or several reject (§11.3), which
/// [`RowLookup`] makes explicit.
#[derive(Debug, Clone)]
pub struct RowSource {
    view: String,
    key_field: String,
}

/// The outcome of resolving a keyed row through a [`RowSource`].
#[derive(Debug, Clone)]
pub enum RowLookup {
    /// No row carried the requested key.
    Missing,
    /// Exactly one row matched.
    Found(ViewRow),
    /// More than one row matched — an ambiguous, rejectable resolution (§11.3).
    Ambiguous,
}

impl RowSource {
    /// A source reading collection rows from the view named `view`, keyed by its
    /// `key_field`.
    #[must_use]
    pub fn new(view: impl Into<String>, key_field: impl Into<String>) -> Self {
        Self { view: view.into(), key_field: key_field.into() }
    }

    /// The view this source reads.
    #[must_use]
    pub fn view(&self) -> &str {
        &self.view
    }

    /// The field within each row that holds its lookup key.
    #[must_use]
    pub fn key_field(&self) -> &str {
        &self.key_field
    }

    /// Resolve the single row whose key field equals `key`, enforcing the
    /// exactly-one rule (§11.3).
    ///
    /// # Errors
    /// Propagates a store/engine fault. A view of the source's name that is not
    /// declared resolves to [`RowLookup::Missing`] rather than an error, so a
    /// misconfigured source denies rather than crashes.
    pub fn resolve(
        &self,
        reader: &dyn StateReader,
        key: &Value,
    ) -> Result<RowLookup, liasse_runtime::EngineError> {
        let Some(result) = reader.view(&self.view)? else {
            return Ok(RowLookup::Missing);
        };
        Ok(self.match_rows(&result, key))
    }

    fn match_rows(&self, result: &ViewResult, key: &Value) -> RowLookup {
        // §5.6: compare application keys, so a ref-typed lookup value (a session's
        // `account`, projected as `Value::Ref`) matches the target collection's
        // scalar key it dereferences to.
        let needle = application_key(key);
        let mut found: Option<&ViewRow> = None;
        for row in result.rows() {
            if row.field(&self.key_field).map(application_key) == Some(needle) {
                if found.is_some() {
                    return RowLookup::Ambiguous;
                }
                found = Some(row);
            }
        }
        match found {
            Some(row) => RowLookup::Found(row.clone()),
            None => RowLookup::Missing,
        }
    }
}
