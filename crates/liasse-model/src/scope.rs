//! The static [`Scope`] an authored expression is checked against (§6.2).
//!
//! A [`ModelScope`] carries exactly the roots and out-of-band bindings §6.2
//! names: the lexical `.`/`^` context chain, the package root `/`, parameters
//! `@name`, structural bindings `$name`, imports `#name`, and lexical bindings.
//! Everything reachable *from* those (field access, selectors, aggregates) is
//! the [`liasse_expr`] checker's job over the [`RowType`](liasse_expr::RowType)s
//! this scope hands back — so the model only has to describe its shapes, which
//! [`Resolver`](crate::resolve::Resolver) already does.

use std::collections::BTreeMap;

use liasse_expr::{ExprType, HostOp, HostPosition, Scope};

use crate::host::HostDescriptors;

/// A concrete scope built from the model tree for one declaration position.
#[derive(Debug, Clone)]
pub(crate) struct ModelScope {
    /// The lexical current chain, innermost last. `current()` is the last;
    /// `^` walks back toward the front.
    contexts: Vec<ExprType>,
    /// The package root `/`.
    root: Option<ExprType>,
    params: BTreeMap<String, ExprType>,
    structurals: BTreeMap<String, ExprType>,
    imports: BTreeMap<String, ExprType>,
    bindings: BTreeMap<String, ExprType>,
    /// The resolved `$requires` namespaces' pinned signatures (§16.2). Empty for a
    /// scope in a package with no host requirements, so a host call there faults
    /// as an unknown function.
    hosts: HostDescriptors,
    /// Which host effect classes this position admits (§16.3, §8.8). A
    /// view/computed/`$check`/`$normalize` stays `Pure`; a field default opts into
    /// `Write`, where a generated function may run.
    host_position: HostPosition,
}

impl ModelScope {
    /// A scope with an explicit lexical chain (ancestors then current) and a
    /// package root.
    pub(crate) fn nested(contexts: Vec<ExprType>, root: ExprType) -> Self {
        Self {
            contexts,
            root: Some(root),
            params: BTreeMap::new(),
            structurals: BTreeMap::new(),
            imports: BTreeMap::new(),
            bindings: BTreeMap::new(),
            hosts: HostDescriptors::default(),
            host_position: HostPosition::Pure,
        }
    }

    /// Attach the resolved host-namespace signatures a call site type-checks
    /// against (§16.2), so `namespace.function(...)` resolves its pinned contract
    /// instead of faulting as an unknown function.
    pub(crate) fn with_host_ops(mut self, hosts: HostDescriptors) -> Self {
        self.hosts = hosts;
        self
    }

    /// Set the host effect policy of this checking position (§16.3, §8.8): a field
    /// default is a `Write` position (a generated function may run); a
    /// view/computed/`$check`/`$normalize` stays the default `Pure`.
    pub(crate) fn with_host_position(mut self, position: HostPosition) -> Self {
        self.host_position = position;
        self
    }

    /// Add a parameter binding `@name`.
    pub(crate) fn with_param(mut self, name: impl Into<String>, ty: ExprType) -> Self {
        self.params.insert(name.into(), ty);
        self
    }

    /// Add a structural binding `$name` (e.g. `$source`, `$from`, `$config`).
    pub(crate) fn with_structural(mut self, name: impl Into<String>, ty: ExprType) -> Self {
        self.structurals.insert(name.into(), ty);
        self
    }

    /// Add the structural binding `$name` when `ty` is present, leaving the scope
    /// unchanged otherwise. Used to bind a module package's `$config` (§13.1) into
    /// every authored-expression scope of a module — and nothing at all in an
    /// application or a module that declares no `$config`.
    pub(crate) fn with_optional_structural(self, name: &str, ty: Option<&ExprType>) -> Self {
        match ty {
            Some(ty) => self.with_structural(name, ty.clone()),
            None => self,
        }
    }

    /// Add a lexical binding — a bare name introduced by a filter or a
    /// `$recursive` `$bind` (§10.5) that names one candidate descendant row.
    pub(crate) fn with_binding(mut self, name: impl Into<String>, ty: ExprType) -> Self {
        self.bindings.insert(name.into(), ty);
        self
    }
}

impl Scope for ModelScope {
    fn current(&self) -> Option<ExprType> {
        self.contexts.last().cloned()
    }

    fn parent(&self, depth: u32) -> Option<ExprType> {
        self.contexts
            .len()
            .checked_sub(1 + depth as usize)
            .and_then(|index| self.contexts.get(index))
            .cloned()
    }

    fn root(&self) -> Option<ExprType> {
        self.root.clone()
    }

    fn param(&self, name: &str) -> Option<ExprType> {
        self.params.get(name).cloned()
    }

    fn structural(&self, name: &str) -> Option<ExprType> {
        self.structurals.get(name).cloned()
    }

    fn import(&self, name: &str) -> Option<ExprType> {
        self.imports.get(name).cloned()
    }

    fn binding(&self, name: &str) -> Option<ExprType> {
        self.bindings.get(name).cloned()
    }

    fn namespace_op(&self, namespace: &str, function: &str) -> Option<HostOp> {
        self.hosts.op(namespace, function).cloned()
    }

    fn host_position(&self) -> HostPosition {
        self.host_position
    }
}
