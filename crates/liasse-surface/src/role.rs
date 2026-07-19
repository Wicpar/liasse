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

use crate::authn::RowSource;
use crate::reader::StateReader;

/// A role definition: the authenticators it accepts and the view that decides its
/// membership. A role is either package-level (its `.` is the package root) or
/// SCOPED — nested on a collection row, so membership is decided *per scope row*
/// (§10.3: "Their location defines scope"). A scoped role's membership view carries
/// a `scope_field` column projecting each grant's role-holding-row key, so
/// membership is confirmed for the specific row a request addresses, not merely for
/// the role held under some other row.
#[derive(Debug, Clone)]
pub struct Role {
    name: String,
    accepted: Vec<String>,
    members: RowSource,
    /// The membership-view column that projects each grant's role-holding-row key,
    /// for a SCOPED role (§10.3). `None` for a package-level role, whose membership
    /// is scope-independent.
    scope_field: Option<String>,
}

impl Role {
    /// A package-level role named `name` accepting the authenticators in `accepted`
    /// (§11.4), whose members are the actor keys projected by `members` (§10.3). Its
    /// membership is scope-independent — the same grant admits every surface.
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
            scope_field: None,
        }
    }

    /// A SCOPED role named `name` (§10.3): nested on a collection row, so membership
    /// is decided per scope row. Its `members` view projects, alongside each grant's
    /// actor key, the role-holding-row key under `scope_field`; admission confirms
    /// membership for the exact row a request addresses (§10.3/§10.5).
    #[must_use]
    pub fn scoped(
        name: impl Into<String>,
        accepted: impl IntoIterator<Item = String>,
        members: RowSource,
        scope_field: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            accepted: accepted.into_iter().collect(),
            members,
            scope_field: Some(scope_field.into()),
        }
    }

    /// Whether this is a scoped role (§10.3), whose membership is decided per scope
    /// row rather than package-wide.
    #[must_use]
    pub fn is_scoped(&self) -> bool {
        self.scope_field.is_some()
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

    /// §10.3: whether `actor` holds the role at the current admission position,
    /// under the scope `scope` addresses. The actor holds it when its exact row
    /// identity occurs at least once in `$members`; repeated occurrences grant no
    /// extra authority.
    ///
    /// For a SCOPED role (§10.3), membership is decided *per scope row*: a non-empty
    /// `scope` confirms the grant is recorded for that exact role-holding row (the
    /// membership view's `scope_field` equals the scope key), so a holder scoped to
    /// row A is not admitted to row B. An empty `scope` on a scoped role asks only
    /// whether the actor holds the role under *any* row (the enumeration-safe
    /// manifest question, §12.1). A package-level role ignores `scope` entirely —
    /// its membership is scope-independent.
    ///
    /// # Errors
    /// Propagates a store/engine fault from evaluating the membership view.
    pub fn holds(
        &self,
        actor: &Value,
        scope: &[Value],
        reader: &dyn StateReader,
    ) -> Result<bool, EngineError> {
        let Some(result) = reader.view(self.members.view())? else {
            // A membership view that is not declared grants the role to no one.
            return Ok(false);
        };
        // §5.6/§5.4/§10.3: membership compares the actor's exact row identity
        // against the projected `$members` view. A single-field key compares by
        // application key, so a ref column (the spec's own §10.3 example projects
        // `.account`, a `$ref`) matches its target's typed key; a composite-keyed
        // actor (§5.4) matches by its positional-tuple row identity — the same
        // split `$actor`/`$session` resolution uses (`RowSource::contains`).
        //
        // §10.3: a scoped role addressed under a specific row additionally requires
        // the grant's projected `scope_field` to equal that row's key, so the same
        // role name grants under one scope row and denies under another.
        match (&self.scope_field, scope.first()) {
            (Some(scope_field), Some(scope_key)) => {
                Ok(self.members.contains_scoped(&result, scope_field, scope_key, actor))
            }
            _ => Ok(self.members.contains(&result, actor)),
        }
    }
}
