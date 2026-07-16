//! The static [`Scope`] the runtime re-checks a `$mut` value expression or a
//! `$view` against (§6.2), rebuilt from the [`Schema`](crate::schema::Schema)
//! because the model keeps no typed program.

use std::collections::BTreeMap;

use liasse_expr::{ExprType, Scope};

/// A concrete scope: a lexical current chain (ancestors then current), the
/// package root `/`, and the parameter contract of the mutation or view.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeScope {
    contexts: Vec<ExprType>,
    root: ExprType,
    params: BTreeMap<String, ExprType>,
    structurals: BTreeMap<String, ExprType>,
}

impl RuntimeScope {
    /// A scope whose innermost `.` is `current`, over the package `root`.
    pub(crate) fn new(current: ExprType, root: ExprType) -> Self {
        Self {
            contexts: vec![current],
            root,
            params: BTreeMap::new(),
            structurals: BTreeMap::new(),
        }
    }

    /// Bind a parameter `@name` to its contract type (§8.3).
    pub(crate) fn with_param(mut self, name: impl Into<String>, ty: ExprType) -> Self {
        self.params.insert(name.into(), ty);
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
        self.structurals.get(name).cloned()
    }

    fn import(&self, _name: &str) -> Option<ExprType> {
        None
    }

    fn binding(&self, _name: &str) -> Option<ExprType> {
        None
    }
}
