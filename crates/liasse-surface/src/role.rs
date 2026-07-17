//! Roles and membership (SPEC.md §10.3).
//!
//! A role combines the authenticators its surfaces accept (§11.4) with the actor
//! rows that hold it (`$members`, §10.3). The model validates the shape of both
//! but leaves `$members` execution a documented seam (its `$actor` row type is a
//! later pass, `crates/liasse-model/src/surface.rs`). This layer supplies the
//! seam: membership is a view of actor keys, and an actor holds the role when its
//! exact identity appears in that view — re-evaluated at every admission, so a
//! role change (a disabled account, a removed membership) takes effect on the
//! next request in serial order (§10.3 "re-evaluated at admission").

use liasse_runtime::{EngineError, Value};

use crate::authn::{application_key, RowSource};
use crate::reader::StateReader;

/// A scoped-role definition: the authenticators it accepts and the view that
/// decides its membership.
#[derive(Debug, Clone)]
pub struct Role {
    name: String,
    accepted: Vec<String>,
    members: RowSource,
}

impl Role {
    /// A role named `name` accepting the authenticators in `accepted` (§11.4),
    /// whose members are the actor keys projected by `members` (§10.3).
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        accepted: impl IntoIterator<Item = String>,
        members: RowSource,
    ) -> Self {
        Self {
            name: name.into(),
            accepted: accepted.into_iter().collect(),
            members,
        }
    }

    /// The role's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The membership view this role resolves against.
    #[must_use]
    pub fn members(&self) -> &RowSource {
        &self.members
    }

    /// The authenticator names this role accepts (§11.4).
    pub fn accepted_names(&self) -> impl Iterator<Item = &String> {
        self.accepted.iter()
    }

    /// §11.4: whether the role accepts the authenticator named `auth`. A role
    /// that accepts exactly one still requires the request to name it.
    #[must_use]
    pub fn accepts(&self, auth: &str) -> bool {
        self.accepted.iter().any(|name| name == auth)
    }

    /// §10.3: whether `actor` holds the role at the current admission position.
    /// The actor holds it when its exact row identity occurs at least once in
    /// `$members`; repeated occurrences grant no extra authority.
    ///
    /// # Errors
    /// Propagates a store/engine fault from evaluating the membership view.
    pub fn holds(&self, actor: &Value, reader: &dyn StateReader) -> Result<bool, EngineError> {
        let Some(result) = reader.view(self.members.view())? else {
            // A membership view that is not declared grants the role to no one.
            return Ok(false);
        };
        // §5.6/§10.3: a `$members` projection may be a ref column (the spec's own
        // §10.3 example projects `.account`, a `$ref`); a ref's application value is
        // its target's typed key, so compare row field and actor by application key
        // — the same rule `$session.account` -> `$actor` resolution uses.
        let actor_key = application_key(actor);
        Ok(result
            .rows()
            .iter()
            .any(|row| row.field(self.members.key_field()).map(application_key) == Some(actor_key)))
    }
}
