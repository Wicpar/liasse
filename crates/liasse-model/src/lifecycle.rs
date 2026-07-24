//! The reserved `module` lifecycle namespace (§13.10).
//!
//! A host-privileged builtin mutation carries a module instance through its
//! lifecycle within a transition: `module.install(...)`, `module.update(...)`, or
//! `module.remove(...)`. This module owns the house-convention names — the reserved
//! namespace and the operation vocabulary — so the runtime interpreter and the
//! module host classify a lifecycle call against one authoritative source rather
//! than duplicating string literals. The privilege enforcement and the actual
//! mount/migration/removal live in the runtime; this is only the naming boundary.

/// The reserved namespace a lifecycle mutation addresses (`module.install(...)`).
/// A package cannot declare a collection or handle that shadows it in call
/// position, so a `module.<op>(...)` call is unambiguously a lifecycle builtin.
pub const LIFECYCLE_NAMESPACE: &str = "module";

/// A module-lifecycle operation a host-privileged builtin mutation performs
/// (§13.10). Each decodes a package definition from a blob (install/update) or
/// addresses an installed instance by handle (remove).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleOp {
    /// Install a new instance from a blob-decoded package (§13.3).
    Install,
    /// Update an existing instance to a blob-decoded package, walking the §20.1
    /// migration chain to the target version (§13.14).
    Update,
    /// Remove an existing instance (§13.12).
    Remove,
}

impl LifecycleOp {
    /// Classify a `module.<member>` call member as a lifecycle operation, or `None`
    /// when the member is not one of the reserved operation names. The caller has
    /// already matched the [`LIFECYCLE_NAMESPACE`].
    #[must_use]
    pub fn classify(member: &str) -> Option<Self> {
        match member {
            "install" => Some(Self::Install),
            "update" => Some(Self::Update),
            "remove" => Some(Self::Remove),
            _ => None,
        }
    }

    /// The reserved member name of this operation (`install`/`update`/`remove`).
    #[must_use]
    pub fn member(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
            Self::Remove => "remove",
        }
    }
}
