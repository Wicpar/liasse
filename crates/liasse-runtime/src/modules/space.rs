//! The mount point of a module space (§13.2).

use crate::modules::ModuleError;

/// The row-scoped location a set of independently configured module instances is
/// installed under (§13.2), e.g. `/companies/acme/modules`. The same package
/// installed in two spaces (`/companies/acme/modules`, `/companies/globex/modules`)
/// yields two independent instances (§13.2 "Installing the same package in each
/// space creates two independent instances").
///
/// A space is the containing-row identity together with the module-space
/// declaration path; with the instance name it forms the local part of instance
/// identity (§13.3: "the containing row identity, module-space declaration path,
/// and instance name"). This type carries the canonical mount path; matching a
/// space against the root package's declared `$modules` mount points and checking
/// the containing row exists is a documented seam (it needs a root-model accessor
/// this crate cannot add).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleSpace {
    path: String,
    components: Vec<String>,
}

impl ModuleSpace {
    /// Parse an absolute mount path into a module space. A module-space location is
    /// rooted at `/` and names at least one component (`/companies/acme/modules`);
    /// a relative, empty, or trailing-slash path is not a module-space location and
    /// is rejected as [`ModuleError::InvalidSpace`].
    pub fn new(path: impl Into<String>) -> Result<Self, ModuleError> {
        let path = path.into();
        let Some(body) = path.strip_prefix('/') else {
            return Err(ModuleError::InvalidSpace(path));
        };
        let components: Vec<String> = body.split('/').map(str::to_owned).collect();
        if components.iter().any(String::is_empty) {
            return Err(ModuleError::InvalidSpace(path));
        }
        Ok(Self { path, components })
    }

    /// The canonical absolute mount path (`/companies/acme/modules`).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.path
    }

    /// The path components in order (`["companies", "acme", "modules"]`).
    #[must_use]
    pub fn components(&self) -> &[String] {
        &self.components
    }
}
