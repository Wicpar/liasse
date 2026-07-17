//! The resolved `$requires` host-namespace signatures a package's expressions
//! type-check against (§16.2).
//!
//! liasse-model does not resolve namespace *contracts* — matching a `$requires`
//! entry to a registered component and pinning its descriptor is the runtime's
//! load-time job against the host registry (§16.2, §9.2 step 4). The Phase-2
//! checker only needs the resolved *shape* of each declared function: the pinned
//! [`HostOp`] the expression checker types a `namespace.function(...)` call site
//! against. The runtime hands these in through
//! [`Model::build_with_hosts`](crate::Model::build_with_hosts), so a
//! `$view`/`$default`/computed/`$check`/`$normalize` host call checks against its
//! declared contract instead of faulting as an unknown function.

use std::collections::BTreeMap;

use liasse_expr::HostOp;

/// The resolved `$requires` namespaces' function signatures (§16.2), keyed by
/// local namespace then function.
///
/// Empty for a package that declares no host requirement — the plain
/// [`Model::build`](crate::Model::build) path, where a host call faults as an
/// unknown function exactly as before this seam existed.
#[derive(Debug, Default, Clone)]
pub struct HostDescriptors(BTreeMap<String, BTreeMap<String, HostOp>>);

impl HostDescriptors {
    /// Assemble descriptors from a `namespace -> function -> op` map — the
    /// runtime's resolved `$requires` signatures, translated once at load.
    #[must_use]
    pub fn new(namespaces: BTreeMap<String, BTreeMap<String, HostOp>>) -> Self {
        Self(namespaces)
    }

    /// The pinned op of `namespace.function`, if the package declares it (§16.2).
    /// `None` for an undeclared namespace or function, which the checker turns
    /// into an "unknown function" diagnostic.
    pub(crate) fn op(&self, namespace: &str, function: &str) -> Option<&HostOp> {
        self.0.get(namespace)?.get(function)
    }
}
