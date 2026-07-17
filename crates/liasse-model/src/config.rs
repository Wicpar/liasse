//! A module package's declared `$config` struct schema (SPEC.md §13.1).
//!
//! §13.1: a module's top-level `$config` is "an immutable typed struct for
//! installation values; defaults use the ordinary field rules, and module
//! expressions read it through `$config`". The model builds and validates that
//! struct once ([`crate::build`]), resolves it to a keyless struct row, and
//! retains it here so two later consumers can rely on one proof-carrying schema:
//!
//! * the model's own expression phases bind `$config` to [`Self::row_type`] in a
//!   module package's authored expressions, so a child `$config`/`$config.member`
//!   read type-checks against the declared members (§13.1);
//! * the composition runtime type-checks an installation's supplied `$config`
//!   values against [`Self::member_type`] (rejecting an unknown member or a type
//!   mismatch, §13.3), evaluates an omitted member's [`Self::default`], and binds
//!   the resulting struct as the `$config` structural value the child reads.

use std::collections::BTreeMap;

use liasse_expr::{ExprType, RowType};

use crate::state::ExprSource;

/// The validated `$config` struct schema of a module package (§13.1): the typed
/// members an installation supplies values for and a child's expressions read
/// through `$config`.
///
/// A `ConfigSchema` is proof the declaration is a well-formed struct of typed
/// value fields — every member's type parsed and every member is an installation
/// value, not a view or keyed collection. The runtime needs no further static
/// check of the *declaration*; it checks only the supplied *values* against it.
#[derive(Debug, Clone)]
pub struct ConfigSchema {
    /// The declared members as a keyless struct row (name → type), in canonical
    /// field-name order.
    row: RowType,
    /// Each member that declares a default, by name (§13.1: "defaults use the
    /// ordinary field rules"). A member with a default MAY be omitted by an
    /// installation; a member absent from this map is required.
    defaults: BTreeMap<String, ExprSource>,
}

impl ConfigSchema {
    /// Assemble a schema from its resolved struct row and per-member defaults.
    pub(crate) fn new(row: RowType, defaults: BTreeMap<String, ExprSource>) -> Self {
        Self { row, defaults }
    }

    /// The declared struct row (§13.1). A module expression scope binds `$config`
    /// to `ExprType::Row(schema.row_type().clone())` so `$config` reads as the
    /// whole struct and `$config.member` reads the member's type; the runtime
    /// supplies the matching installation values as the `$config` structural cell.
    #[must_use]
    pub fn row_type(&self) -> &RowType {
        &self.row
    }

    /// The declared type of one config member, or `None` when the struct declares
    /// no such member. An installation checks a supplied `$config` member with
    /// this: `None` is an unknown member (§13.1, rejected); otherwise the supplied
    /// value's type must match (§13.3).
    #[must_use]
    pub fn member_type(&self, name: &str) -> Option<&ExprType> {
        self.row.field(name)
    }

    /// The declared members in canonical field-name order (name → type), so a
    /// consumer can iterate the whole struct — e.g. to confirm every required
    /// member was supplied.
    pub fn members(&self) -> impl Iterator<Item = (&String, &ExprType)> {
        self.row.fields()
    }

    /// The default expression a member declares, when it has one (§13.1). An
    /// installation that omits the member evaluates this default to obtain its
    /// value; a member with no default is required. `None` also for an unknown
    /// member.
    #[must_use]
    pub fn default(&self, name: &str) -> Option<&ExprSource> {
        self.defaults.get(name)
    }
}
