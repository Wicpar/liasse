//! The static [`Scope`] the runtime re-checks a `$mut` value expression or a
//! `$view` against (§6.2), rebuilt from the [`Schema`](crate::schema::Schema)
//! because the model keeps no typed program.

use std::collections::BTreeMap;

use liasse_expr::{ExprType, HostOp, HostPosition, Scope};

use crate::host::HostSignatures;

/// A concrete scope: a lexical current chain (ancestors then current), the
/// package root `/`, the parameter contract of the mutation or view, and the
/// resolved host-namespace signatures a `namespace.function(...)` call is typed
/// against (§16.2) with the effect policy of this checking position (§16.3).
#[derive(Debug, Clone)]
pub(crate) struct RuntimeScope {
    contexts: Vec<ExprType>,
    root: ExprType,
    params: BTreeMap<String, ExprType>,
    structurals: BTreeMap<String, ExprType>,
    bindings: BTreeMap<String, ExprType>,
    /// The resolved `$requires` namespaces' pinned signatures (§16.2). Empty for a
    /// scope in a package with no host requirements, so a host call there faults
    /// as an unknown function.
    hosts: HostSignatures,
    /// Which host effect classes this position admits (§16.3, §8.8). A view/check
    /// stays `Pure`; a field default or mutation value opts into `Write`.
    host_position: HostPosition,
}

impl RuntimeScope {
    /// A scope whose innermost `.` is `current`, over the package `root`.
    pub(crate) fn new(current: ExprType, root: ExprType) -> Self {
        Self {
            contexts: vec![current],
            root,
            params: BTreeMap::new(),
            structurals: BTreeMap::new(),
            bindings: BTreeMap::new(),
            hosts: HostSignatures::default(),
            host_position: HostPosition::Pure,
        }
    }

    /// Attach the resolved host-namespace signatures a call site type-checks
    /// against (§16.2), so `namespace.function(...)` resolves its pinned contract.
    pub(crate) fn with_host_ops(mut self, hosts: HostSignatures) -> Self {
        self.hosts = hosts;
        self
    }

    /// Set the host effect policy of this checking position (§16.3, §8.8): a field
    /// default or mutation value is a `Write` position (generated permitted); a
    /// view/check stays the default `Pure`.
    pub(crate) fn with_host_position(mut self, position: HostPosition) -> Self {
        self.host_position = position;
        self
    }

    /// Bind a parameter `@name` to its contract type (§8.3).
    pub(crate) fn with_param(mut self, name: impl Into<String>, ty: ExprType) -> Self {
        self.params.insert(name.into(), ty);
        self
    }

    /// Bind a lexical local `name` (a `name = …` statement binding, §8.1) to its
    /// type, so a later statement or the `return` can reference it (§6.2).
    pub(crate) fn with_binding(mut self, name: impl Into<String>, ty: ExprType) -> Self {
        self.bindings.insert(name.into(), ty);
        self
    }

    /// Bind a structural `$name` (e.g. the `$target` of an `$on_delete` patch,
    /// §21.1) to its type in the current context (§6.2).
    pub(crate) fn with_structural(mut self, name: impl Into<String>, ty: ExprType) -> Self {
        self.structurals.insert(name.into(), ty);
        self
    }
}

impl Scope for RuntimeScope {
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
        Some(self.root.clone())
    }

    fn param(&self, name: &str) -> Option<ExprType> {
        self.params.get(name).cloned()
    }

    fn structural(&self, name: &str) -> Option<ExprType> {
        if let Some(ty) = self.structurals.get(name) {
            return Some(ty.clone());
        }
        // §13.1: a package-wide structural binding (`$config`) is carried on the
        // root row type, so every scope built over this root resolves it without
        // each construction site rebinding it.
        match &self.root {
            ExprType::Row(row) | ExprType::View(row) => row.structural(name).cloned(),
            ExprType::Scalar(_) => None,
        }
    }

    fn import(&self, _name: &str) -> Option<ExprType> {
        None
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
