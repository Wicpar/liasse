//! Resolved authentication identities and the row-lookup they resolve through.
//!
//! There is no separate actor registry (§10.3): an authenticator resolves one
//! ordinary application row as `$actor` and, when declared, one as `$session`.
//! These types are the resolved result, carried for the request's lifetime and
//! re-derived from committed state at every admission.

use liasse_expr::RowId;
use liasse_ident::KeyText;
use liasse_runtime::{RefKey, Timestamp, Value, ViewResult, ViewRow};

use crate::reader::StateReader;

/// A value's *application key* (§5.6): a ref's application value is its target's
/// typed key, so a scalar-keyed ref dereferences to that key; every other value
/// is its own key. This lets a `$session.account` ref (`Value::Ref`) match the
/// accounts collection's scalar key when resolving `$actor` (§11.3). A composite
/// ref keeps its whole value here — [`composite_identity`] is the multi-component
/// counterpart, normalizing it to the positional tuple its target row carries.
pub(crate) fn application_key(value: &Value) -> &Value {
    match value {
        Value::Ref(reference) => match reference.key() {
            RefKey::Scalar(inner) => inner,
            RefKey::Composite(_) => value,
        },
        _ => value,
    }
}

/// The positional composite key identity `value` names, when it is a
/// multi-component key (§5.4): a bare [`Value::Composite`] passes through, and a
/// composite `ref`'s target key ([`RefKey::Composite`]) yields the equal-valued
/// tuple (§5.6, §A.9). A single-component value — any scalar, or a scalar ref —
/// is `None`, so the caller resolves it through single-field-key matching exactly
/// as before. This is the surface counterpart of the runtime's `key_identity`
/// (`crates/liasse-runtime/src/materialize.rs`): both name a composite row by the
/// positional `Value::Composite` tuple in `$key` order.
fn composite_identity(value: &Value) -> Option<Value> {
    match value {
        Value::Composite(_) => Some(value.clone()),
        Value::Ref(reference) => match reference.key() {
            RefKey::Composite(components) => Some(Value::Composite(components.clone())),
            RefKey::Scalar(_) => None,
        },
        _ => None,
    }
}

/// The canonical row identity (Annex D.1/D.2) a view row over `key`'s collection
/// carries: the typed key VALUE in its ref-flattened identity form
/// ([`Value::identity_value`]), wrapped as the same [`RowId`] the runtime's
/// materialization (`crates/liasse-runtime/src/materialize.rs`,
/// `RowId::keyed_value(key.identity_value())`) derives for a materialized row. Both
/// layers flatten a ref component to its target key, so a composite
/// `$actor`/`$session`/member identity matches the stored row by exactly the
/// identity the engine re-materializes it under — whether a ref key component is
/// carried as a `Value::Ref` or as its bare scalar key (§6.3). `KeyText` still
/// validates that `key` has a canonical D.2 rendering (never a non-key value),
/// failing closed to no match otherwise (§6.3).
fn row_identity(key: &Value) -> Option<RowId> {
    KeyText::from_key_values(std::slice::from_ref(key))
        .ok()
        .map(|_| RowId::keyed_value(key.identity_value()))
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

/// Names one application view and the field within it that holds a row's
/// single-field lookup key — the surface layer's stand-in for a
/// `/collection[$key]` selection (§11.3) evaluated through the engine's named-view
/// API. `$session`/`$actor` MUST each resolve exactly one row; zero or several
/// reject (§11.3), which [`RowLookup`] makes explicit.
///
/// A composite-keyed collection (§5.4) has no single field equal to the whole
/// `$key`, so a composite lookup is resolved by the row's canonical identity
/// ([`row_identity`]) rather than `key_field`: the positional `Value::Composite`
/// tuple names one row exactly. `key_field` still governs the single-field-key
/// case unchanged (§5.6), including a ref-typed projection that dereferences to a
/// scalar key.
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

    /// The application key identity to carry for a `row` this source matched
    /// against `lookup` (§11.1, §5.4). A composite key is the positional
    /// [`Value::Composite`] tuple `lookup` names — equal to the matched row's key
    /// by construction — so a downstream admission re-materializes the stored
    /// N-component row rather than a truncated one-component address that would
    /// name no row (fail closed, §6.3). A single-field key is the row's own
    /// `key_field` (§5.6) — not `lookup`, which may be a ref whose scalar target
    /// key the row carries directly.
    pub(crate) fn matched_key(&self, row: &ViewRow, lookup: &Value) -> Value {
        composite_identity(lookup)
            .unwrap_or_else(|| row.field(&self.key_field).cloned().unwrap_or_else(|| lookup.clone()))
    }

    /// Whether any row in `result` records `actor` as a member *scoped to*
    /// `scope_key` (§10.3): a grant whose projected `scope_field` equals the
    /// role-holding-row key and whose `key_field` equals the actor. Both are
    /// compared by application key (§5.6), so a `$ref` column (the actor account, or
    /// a ref-typed scope key) matches its target's typed key. A holder scoped to one
    /// row therefore fails this test for another — the per-scope membership §10.3
    /// requires, closing the cross-scope read/mutation grant.
    pub(crate) fn contains_scoped(
        &self,
        result: &ViewResult,
        scope_field: &str,
        scope_key: &Value,
        actor: &Value,
    ) -> bool {
        let scope_needle = application_key(scope_key);
        let actor_needle = application_key(actor);
        result.rows().iter().any(|row| {
            row.field(scope_field).map(application_key) == Some(scope_needle)
                && row.field(&self.key_field).map(application_key) == Some(actor_needle)
        })
    }

    /// Whether any row in `result` carries the key identity `key` — §10.3
    /// membership ("its exact row identity occurs at least once"; repeated
    /// occurrences grant no extra authority). A composite key matches by row
    /// identity (§5.4), a single-field key by its projected `key_field`
    /// application key (§5.6), the same split [`Self::match_rows`] resolves under.
    pub(crate) fn contains(&self, result: &ViewResult, key: &Value) -> bool {
        match composite_identity(key) {
            Some(identity) => row_identity(&identity)
                .is_some_and(|target| result.rows().iter().any(|row| row.id() == &target)),
            None => {
                let needle = application_key(key);
                result
                    .rows()
                    .iter()
                    .any(|row| row.field(&self.key_field).map(application_key) == Some(needle))
            }
        }
    }

    fn match_rows(&self, result: &ViewResult, key: &Value) -> RowLookup {
        match composite_identity(key) {
            // §5.4: a composite lookup names a positional key identity that no
            // single projected field holds; a view row carries that identity as
            // its canonical `RowId` (D.1), so match the whole tuple by row identity.
            Some(identity) => self.match_by_identity(result, &identity),
            // §5.6: single-field key — compare application keys, so a ref-typed
            // lookup value (a session's `account`, projected as `Value::Ref`)
            // matches the target collection's scalar key it dereferences to.
            None => self.match_by_field(result, key),
        }
    }

    fn match_by_identity(&self, result: &ViewResult, identity: &Value) -> RowLookup {
        let Some(target) = row_identity(identity) else {
            return RowLookup::Missing;
        };
        let mut found: Option<&ViewRow> = None;
        for row in result.rows() {
            if row.id() == &target {
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

    fn match_by_field(&self, result: &ViewResult, key: &Value) -> RowLookup {
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
