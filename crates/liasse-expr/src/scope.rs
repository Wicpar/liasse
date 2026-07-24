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

use crate::host::{DbReadPosition, HostOp, HostPosition};
use crate::ty::ExprType;

/// The typed contract of an interface `$mut` addressed on an imported module
/// instance (§13.8/§13.10): the parameter types the interface prototype supplies
/// and the declared `$return` type. Consulted by the checker to type a
/// `#handle.mutation(args)` dispatch as a well-typed call whose result is its
/// `$return`. The parameter list is empty when the contract declares no explicit
/// prototype (nothing to check); `ret` is the declared response type.
#[derive(Debug, Clone)]
pub struct InterfaceMut {
    /// The declared parameter types the boundary supplies, by name (§13.8).
    pub params: Vec<(String, ExprType)>,
    /// The declared `$return` response type (§13.8) — the result type the call
    /// yields to the caller.
    pub ret: ExprType,
}

/// Resolves the roots and bindings of §6.2 to static types.
///
/// A missing binding returns `None`; the checker turns that into an
/// "unknown name" diagnostic. A structural binding absent from the current
/// feature context (e.g. `$config` outside a module expression) is exactly a
/// `None` from [`Scope::structural`].
pub trait Scope: Send + Sync {
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

    /// Resolve an interface `$mut` contract addressed on an imported instance
    /// `#handle.mutation(...)` (§13.8/§13.10): the parameter types the interface
    /// prototype supplies and the declared `$return` type. `None` when `#handle`
    /// imports no such interface mutation — the checker turns that into an
    /// "unknown interface mutation" diagnostic, so a `#handle.field` view read
    /// still routes to ordinary field access. The default resolves nothing, so a
    /// scope that binds no callable interface sees only ordinary import reads.
    fn interface_mut(&self, handle: &str, mutation: &str) -> Option<InterfaceMut> {
        let _ = (handle, mutation);
        None
    }

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

    /// Which execution context this checking position is (§16.3/§8.8 effect
    /// classes, §16.5 namespace origin). The default is the most restrictive — a
    /// database-evaluated read position, built-in-only and pure-only — so any
    /// scope that admits more (a field default, or the mutation program body where
    /// an app-registered namespace of any effect class may run) opts in
    /// explicitly.
    fn host_position(&self) -> HostPosition {
        HostPosition::DbRead(DbReadPosition::ViewProjection)
    }
}
