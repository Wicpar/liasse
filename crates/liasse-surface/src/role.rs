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

    /// §10.3/§10.4: whether `actor` holds the role for the EXACT scope a request
    /// addresses — the per-scope membership admission gates on. The actor holds it
    /// when its exact row identity occurs at least once in `$members`; repeated
    /// occurrences grant no extra authority. Identity compares by application key
    /// (§5.6/§5.4), so a `$ref` actor column (the spec's own §10.3 example projects
    /// `.account`, a `$ref`) matches its target's typed key, and a composite-keyed
    /// actor matches by its positional-tuple row identity — see
    /// [`RowSource::contains`]/[`RowSource::contains_scoped`].
    ///
    /// For a SCOPED role (§10.3) the grant is confirmed *per scope row*: a non-empty
    /// `scope` carries the addressed role-holding-row key, and membership holds only
    /// when the membership view records that actor under that exact key (its
    /// `scope_field` equals the scope key), so a holder scoped to row A is not
    /// admitted to row B. An EMPTY scope on a scoped role names no row, so it can
    /// confirm no per-scope grant: a scoped surface addressed without its scope key
    /// is UNRESOLVABLE, denied uniformly like a nonexistent address (§10.4). It must
    /// NEVER fall through to the any-row "member somewhere" question — that would let
    /// an empty-scope probe distinguish a member-somewhere from a non-member and leak
    /// self-membership over the wire (the §10.4 oracle). Use [`Self::holds_anywhere`]
    /// for the enumeration-safe manifest question. A package-level role ignores
    /// `scope` — its membership is scope-independent.
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
        match (&self.scope_field, scope.first()) {
            // §10.3/§10.5: a scoped role addressed under a specific row requires the
            // grant's projected `scope_field` to equal that row's key, so the same
            // role name grants under one scope row and denies under another.
            (Some(scope_field), Some(scope_key)) => {
                Ok(self.members.contains_scoped(&result, scope_field, scope_key, actor))
            }
            // §10.4: a scoped role addressed with an EMPTY scope names no row to
            // confirm a grant for — unresolvable, never the any-row grant.
            (Some(_), None) => Ok(false),
            // §10.3: a package-level role is scope-independent.
            (None, _) => Ok(self.members.contains(&result, actor)),
        }
    }

    /// §12.1: whether `actor` holds this role under ANY scope row (a scoped role) or
    /// at all (a package-level role) — the enumeration-safe "member somewhere"
    /// question the manifest lists a role's surfaces on. It confers NO admission on
    /// its own: admission ([`Self::holds`]) still re-confirms the exact scope a
    /// request addresses (§10.3), so this reveals only that the CALLER holds the role
    /// somewhere — never another actor's grant, nor a specific scope's existence.
    ///
    /// # Errors
    /// Propagates a store/engine fault from evaluating the membership view.
    pub fn holds_anywhere(&self, actor: &Value, reader: &dyn StateReader) -> Result<bool, EngineError> {
        let Some(result) = reader.view(self.members.view())? else {
            // A membership view that is not declared grants the role to no one.
            return Ok(false);
        };
        Ok(self.members.contains(&result, actor))
    }
}
