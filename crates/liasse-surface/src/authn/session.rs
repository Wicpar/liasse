//! The session/account authenticator — the §11.5 login pattern in reverse.
//!
//! It mirrors the spec's `session` authenticator (§11.3): verify the credential,
//! confirm the proof is bound to this authenticator, resolve `$session` and
//! `$actor` to exactly one row each, and enforce session activity (§11.7). A
//! stateless variant (no `$session`) covers the `api_key` shape, resolving the
//! actor straight from the proof's account claim.

use liasse_value::Value;

use crate::outcome::{Denial, DenialReason};
use crate::reader::StateReader;

use super::identity::{Actor, AuthContext, RowLookup, RowSource, Session};
use super::{Authenticator, Claims, Credential, Verifier};

/// Where the session row lives and which of its fields carry the account
/// reference, expiry, and revocation state (§11.2 sessions collection).
#[derive(Debug, Clone)]
pub struct SessionSource {
    rows: RowSource,
    account_field: String,
    expires_field: String,
    revoked_field: String,
}

impl SessionSource {
    /// A session source reading `rows` (a view keyed by the session id field),
    /// whose `account_field`/`expires_field`/`revoked_field` name the §11.2
    /// session members.
    #[must_use]
    pub fn new(
        rows: RowSource,
        account_field: impl Into<String>,
        expires_field: impl Into<String>,
        revoked_field: impl Into<String>,
    ) -> Self {
        Self {
            rows,
            account_field: account_field.into(),
            expires_field: expires_field.into(),
            revoked_field: revoked_field.into(),
        }
    }

    /// Resolve, validate, and read a session row keyed by `session_key`. The
    /// returned pair is the active session and its account key.
    fn resolve(
        &self,
        reader: &dyn StateReader,
        session_key: &Value,
    ) -> Result<(Session, Value), Denial> {
        let row = match self.rows.resolve(reader, session_key) {
            Ok(RowLookup::Found(row)) => row,
            Ok(RowLookup::Missing) => {
                return Err(Denial::new(DenialReason::SessionInvalid, "no session matches the proof"));
            }
            Ok(RowLookup::Ambiguous) => {
                return Err(Denial::new(DenialReason::SessionInvalid, "the session key is ambiguous"));
            }
            Err(_) => {
                return Err(Denial::new(DenialReason::SessionInvalid, "the session store is unreadable"));
            }
        };
        // The expiry is the session bucket's upper bound `$until` (§14.1). A
        // finite timestamp bounds the lifetime; an absent field or `none`
        // resolves to an omitted upper bound, leaving the interval unbounded
        // (§14) — a perpetual session, active until revoked (§11.7). A present
        // value of any other type is a malformed row and denies rather than
        // silently reading as perpetual.
        let expires_at = match row.field(&self.expires_field) {
            Some(Value::Timestamp(instant)) => Some(*instant),
            None | Some(Value::None) => None,
            Some(_) => {
                return Err(Denial::new(DenialReason::SessionInvalid, "the session expiry is not a timestamp"));
            }
        };
        // A missing revocation flag defaults to not-revoked (§11.2 `revoked:
        // bool = false`): a session row need not project a false flag.
        let revoked = matches!(row.field(&self.revoked_field), Some(Value::Bool(true)));
        let Some(account) = row.field(&self.account_field).cloned() else {
            return Err(Denial::new(DenialReason::SessionInvalid, "the session names no account"));
        };
        let session = Session::new(session_key.clone(), expires_at, revoked);
        if !session.is_active_at(reader.now()) {
            return Err(Denial::new(DenialReason::SessionInvalid, "the session is revoked or expired"));
        }
        Ok((session, account))
    }
}

/// A session/account authenticator (§11.3).
pub struct SessionAuthenticator {
    name: String,
    verifier: Box<dyn Verifier>,
    session: Option<SessionSource>,
    accounts: RowSource,
}

impl SessionAuthenticator {
    /// A session-backed authenticator: `verifier` produces the proof, `session`
    /// resolves and validates the `$session` row, and `accounts` confirms the
    /// `$actor` row exists (§11.3).
    #[must_use]
    pub fn session(
        name: impl Into<String>,
        verifier: Box<dyn Verifier>,
        session: SessionSource,
        accounts: RowSource,
    ) -> Self {
        Self { name: name.into(), verifier, session: Some(session), accounts }
    }

    /// A stateless authenticator (the `api_key` shape, §11.3): no `$session`; the
    /// actor is resolved straight from the proof's account claim.
    #[must_use]
    pub fn stateless(name: impl Into<String>, verifier: Box<dyn Verifier>, accounts: RowSource) -> Self {
        Self { name: name.into(), verifier, session: None, accounts }
    }

    /// §11.4 proof binding: the proof MUST name this authenticator, or a proof
    /// minted for another could be replayed here.
    fn bound(&self, claims: &Claims) -> Result<(), Denial> {
        if claims.auth() == self.name {
            Ok(())
        } else {
            Err(Denial::new(
                DenialReason::CheckFailed,
                "the proof is not bound to the selected authenticator",
            ))
        }
    }

    /// Resolve `$actor` to exactly one existing row (§11.3). The actor's identity
    /// is the resolved row's own typed key (§5.6) — not the lookup value, which
    /// may be a ref (a session's `account`) — so a downstream admission that
    /// re-materializes `$actor` by key finds the account row.
    fn actor(&self, reader: &dyn StateReader, account: &Value) -> Result<Actor, Denial> {
        match self.accounts.resolve(reader, account) {
            Ok(RowLookup::Found(row)) => {
                let key = row.field(self.accounts.key_field()).cloned().unwrap_or_else(|| account.clone());
                Ok(Actor::new(key))
            }
            Ok(RowLookup::Missing) => {
                Err(Denial::new(DenialReason::ActorUnresolved, "no account matches the proof"))
            }
            Ok(RowLookup::Ambiguous) => {
                Err(Denial::new(DenialReason::ActorUnresolved, "the account key is ambiguous"))
            }
            Err(_) => Err(Denial::new(DenialReason::ActorUnresolved, "the account store is unreadable")),
        }
    }
}

impl Authenticator for SessionAuthenticator {
    fn name(&self) -> &str {
        &self.name
    }

    fn resolve(&self, credential: &Credential, reader: &dyn StateReader) -> Result<AuthContext, Denial> {
        let claims = self
            .verifier
            .verify(credential)
            .map_err(|failure| Denial::new(DenialReason::Unverified, failure.to_string()))?;
        self.bound(&claims)?;

        match &self.session {
            Some(source) => {
                let Some(session_key) = claims.session() else {
                    return Err(Denial::new(
                        DenialReason::SessionInvalid,
                        "the proof carries no session key",
                    ));
                };
                let (session, account) = source.resolve(reader, session_key)?;
                let actor = self.actor(reader, &account)?;
                Ok(AuthContext::new(&self.name, actor, Some(session)))
            }
            None => {
                let Some(account) = claims.account() else {
                    return Err(Denial::new(
                        DenialReason::ActorUnresolved,
                        "the proof carries no account key",
                    ));
                };
                let actor = self.actor(reader, account)?;
                Ok(AuthContext::new(&self.name, actor, None))
            }
        }
    }
}
