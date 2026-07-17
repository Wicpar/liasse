//! The static scope an expression is type-checked against.
//!
//! [`Scope`] is the typing counterpart of [`Environment`](crate::Environment):
//! it resolves only the *roots and out-of-band bindings* of §6.2 to their
//! [`ExprType`]. Everything reachable *from* a root — field access, selectors,
//! projections, aggregates — is walked by the type checker over the returned
//! [`RowType`](crate::RowType), so a scope stays small and a host (liasse-model)
//! implements it by exposing its state-tree shapes, parameter types, and
//! receiver type.
//!
//! Row bindings introduced *inside* an expression (a `[:name | …]` filter, a
//! `::` traversal, a projection self-reference) are the checker's own concern
//! and never reach the scope.

use crate::host::{HostOp, HostPosition};
use crate::ty::ExprType;

/// Resolves the roots and bindings of §6.2 to static types.
///
/// A missing binding returns `None`; the checker turns that into an
/// "unknown name" diagnostic. A structural binding absent from the current
/// feature context (e.g. `$config` outside a module expression) is exactly a
/// `None` from [`Scope::structural`].
pub trait Scope {
    /// The type of the current value or row `.` (§6.2).
    fn current(&self) -> Option<ExprType>;

    /// The type of the lexical parent scope at `depth` (`^` is depth 1, `^^`
    /// depth 2, …) (§6.2).
    fn parent(&self, depth: u32) -> Option<ExprType>;

    /// The type of the package root `/` (§6.2).
    fn root(&self) -> Option<ExprType>;

    /// The type of a mutation or view parameter `@name` (§6.2, §8.3).
    fn param(&self, name: &str) -> Option<ExprType>;

    /// The type of a structural binding `$name` in the current context (§6.2).
    fn structural(&self, name: &str) -> Option<ExprType>;

    /// The type of an imported module or parent surface `#name` (§6.2).
    fn import(&self, name: &str) -> Option<ExprType>;

    /// The type of a lexical local or row binding `name` visible from the
    /// enclosing declaration (§6.2). Bindings introduced *within* the
    /// expression are resolved by the checker before it consults the scope.
    fn binding(&self, name: &str) -> Option<ExprType>;

    /// Resolve a declared host-namespace function's pinned signature (§16.2):
    /// `namespace` is the local `$requires` key (the expression namespace),
    /// `function` the called function. `None` when the package declares no such
    /// namespace or the namespace declares no such function — the checker turns
    /// that into an "unknown function" diagnostic (a host call must name an
    /// explicitly declared requirement, §16.2). The default resolves nothing, so
    /// a scope that carries no host requirements sees only the core namespaces.
    fn namespace_op(&self, namespace: &str, function: &str) -> Option<HostOp> {
        let _ = (namespace, function);
        None
    }

    /// Which host effect classes this checking position admits (§16.3, §8.8).
    /// The default is the most restrictive — a pure read/replay position — so
    /// any scope that permits an effect (a field default, a mutation value, an
    /// admission verifier) opts in explicitly.
    fn host_position(&self) -> HostPosition {
        HostPosition::Pure
    }
}
