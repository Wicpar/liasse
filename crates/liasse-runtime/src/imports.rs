//! The §13.4 parent-provided surfaces a child module instance imports.
//!
//! A module space MAY project a parent capability to its children through the
//! `$modules` `$expose` block (§13.4). A child imports one under `$use`
//! (`company: "$parent"`, or renamed `org: "$parent.company"`) and reads or calls
//! it through `#company`/`#org`. The surface is **row-local**: under Acme's space
//! it refers to Acme, under Globex to Globex — the projection is evaluated against
//! the module space's containing row (§13.4).
//!
//! Each bound handle carries two faces of the same projection: the row **type**
//! the child's compile and its `$data` seed check type an import read against
//! (`#company.plan`), and the row **value** those same reads evaluate against
//! (the containing row projected through the parent's `$expose` `$view`). The
//! host resolves both from its root engine at install (and re-resolves the value
//! live for a child interface read, so a later parent mutation is observed).

use std::collections::BTreeMap;

use liasse_expr::{Cell, ExprType};

/// The parent-surface projections bound to a child's `#import` handles (§13.4),
/// keyed by the child-visible handle name. Empty for a standalone load, a root
/// read, or a migration — every evaluation that imports no parent surface.
#[derive(Debug, Clone, Default)]
pub(crate) struct ParentImports {
    /// The projected row value each handle reads (`#company` → the containing row
    /// projected through the parent `$expose` `$view`).
    values: BTreeMap<String, Cell>,
    /// The row type each handle's reads are typed against at the child compile and
    /// the child `$data` seed check.
    types: BTreeMap<String, ExprType>,
}

/// The shared empty projection every non-module evaluation carries: no import
/// handle is bound, so a stray `#name` read faults as an unknown import exactly
/// as it did before parent surfaces existed.
pub(crate) static EMPTY: ParentImports = ParentImports { values: BTreeMap::new(), types: BTreeMap::new() };

/// One parent surface resolved row-local against a module space's containing row
/// (§13.4): the row `ty` a child types its imported reads against, the projected
/// row `value` those reads evaluate against, and the `$mut` bindings a child's
/// `#handle.mutation(...)` routes to the containing-row mutation through.
pub(crate) struct ResolvedParentSurface {
    /// The row type of the parent `$expose` `$view` projection.
    pub(crate) ty: ExprType,
    /// The projected row value (the containing row through the `$view`).
    pub(crate) value: Cell,
    /// The `(contract, containing-row binding)` mutation pairs (`("rename",
    /// ".rename")`) the surface projects (§13.4).
    pub(crate) muts: Vec<(String, String)>,
}

impl ParentImports {
    /// Bind one handle to its resolved parent surface (§13.4): the row `ty` the
    /// child types its imported reads against and the projected row `value` those
    /// reads evaluate against.
    pub(crate) fn bind(&mut self, handle: impl Into<String>, ty: ExprType, value: Cell) {
        let handle = handle.into();
        self.types.insert(handle.clone(), ty);
        self.values.insert(handle, value);
    }

    /// The bound handle → row-value map the evaluation environment answers a
    /// `#handle` read from (§13.4).
    pub(crate) fn values(&self) -> &BTreeMap<String, Cell> {
        &self.values
    }

    /// The bound handle → row-type map the compile/seed-check scope types a
    /// `#handle` read against (§13.4).
    pub(crate) fn types(&self) -> &BTreeMap<String, ExprType> {
        &self.types
    }
}
