//! The install request and the boundary bindings an admitted instance records
//! (§13.3, §13.5, §13.6).

use std::collections::BTreeMap;

use liasse_value::Value;

use crate::modules::ModuleError;

/// A `$use` binding target (§13.4/§13.5): a parent capability, a renamed parent
/// surface, a resolved sibling path, or an unresolved peer requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseSpec {
    /// `$parent`: the module space's projected parent capability under the same
    /// handle name (§13.4).
    Parent,
    /// `$parent.<name>`: a parent capability imported under a renamed handle
    /// (§13.4 "Renaming the handle").
    ParentSurface(String),
    /// An absolute sibling path (`/companies/acme/modules/people`), optionally
    /// `#interface`-qualified — the concrete binding an operator supplies when peer
    /// resolution is ambiguous (§13.3 `$resolved`).
    Path(String),
    /// A peer requirement `line/interface@major` (§13.5): the package line before
    /// `/`, the exposed interface after `/`, and the compatible major after `@`.
    /// Resolution against the installed sibling set is a documented seam.
    Peer { line: String, interface: String, major: u64 },
}

impl UseSpec {
    /// Parse a `$use` spec string (§13.4/§13.5). A malformed spec is rejected as
    /// [`ModuleError::InvalidBinding`] so the recorded binding is proof-carrying.
    pub fn parse(spec: &str) -> Result<Self, ModuleError> {
        if spec == "$parent" {
            return Ok(Self::Parent);
        }
        if let Some(name) = spec.strip_prefix("$parent.") {
            return non_empty(name).map(|n| Self::ParentSurface(n.to_owned()));
        }
        if spec.starts_with('/') {
            return non_empty(spec).map(|s| Self::Path(s.to_owned()));
        }
        parse_peer(spec)
    }
}

/// Parse a peer spec `line/interface@major` (§13.5).
fn parse_peer(spec: &str) -> Result<UseSpec, ModuleError> {
    let (line, rest) = spec.split_once('/').ok_or_else(|| ModuleError::InvalidBinding(spec.to_owned()))?;
    let (interface, major) = rest.split_once('@').ok_or_else(|| ModuleError::InvalidBinding(spec.to_owned()))?;
    let major: u64 = major.parse().map_err(|_| ModuleError::InvalidBinding(spec.to_owned()))?;
    if line.is_empty() || interface.is_empty() {
        return Err(ModuleError::InvalidBinding(spec.to_owned()));
    }
    Ok(UseSpec::Peer { line: line.to_owned(), interface: interface.to_owned(), major })
}

fn non_empty(text: &str) -> Result<&str, ModuleError> {
    if text.is_empty() {
        Err(ModuleError::InvalidBinding(text.to_owned()))
    } else {
        Ok(text)
    }
}

/// A `$deps` binding (§13.6): a private package requirement owned by the consumer,
/// naming the package line and the compatible major. A `$deps` entry creates a
/// private nested instance siblings cannot address; provisioning that nested
/// instance is a documented seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepSpec {
    pub line: String,
    pub major: u64,
}

impl DepSpec {
    /// Parse a `$deps` spec `line@major` (§13.6).
    pub fn parse(spec: &str) -> Result<Self, ModuleError> {
        let (line, major) = spec.split_once('@').ok_or_else(|| ModuleError::InvalidBinding(spec.to_owned()))?;
        let major: u64 = major.parse().map_err(|_| ModuleError::InvalidBinding(spec.to_owned()))?;
        if line.is_empty() {
            return Err(ModuleError::InvalidBinding(spec.to_owned()));
        }
        Ok(Self { line: line.to_owned(), major })
    }
}

/// The boundary bindings an admitted instance records (§13.3: "The admitted
/// instance records the exact package and every resolved choice"). Kept beside the
/// child engine so disable/enable and aggregation can reason about the boundary
/// occurrences (§13.12) without re-parsing the request.
#[derive(Debug, Clone, Default)]
pub struct AdmittedBindings {
    /// The immutable `$config` installation values (§13.1). Recorded here; reading
    /// them through `$config` inside child expressions is a documented seam (the
    /// expression language has no `$config` binding yet).
    pub config: BTreeMap<String, Value>,
    /// Required and optional `$use` handles (§13.5), each carrying whether its
    /// absence is valid (`$optional`).
    pub uses: Vec<(String, UseSpec, bool)>,
    /// Private `$deps` requirements (§13.6).
    pub deps: Vec<(String, DepSpec)>,
}

/// A §13.3 module install request: the instance name, the child package
/// definition to load, the immutable `$config`, and the explicit `$use`/`$deps`
/// boundary bindings. Built fluently; the host admits it into a module space,
/// recording the bindings on the new instance.
pub struct InstallRequest {
    name: String,
    definition: String,
    bindings: AdmittedBindings,
    /// The installation `$data` overlay (§13.3), as the JSON text of the `$data`
    /// object. Applied onto the child genesis after the package `$data` seed.
    data: Option<String>,
    error: Option<ModuleError>,
}

impl InstallRequest {
    /// A request to install `definition` as instance `name` (§13.3). The name is
    /// the local component of instance identity; an empty name is rejected at
    /// admission (§13.3 "a non-empty text value").
    #[must_use]
    pub fn new(name: impl Into<String>, definition: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            definition: definition.into(),
            bindings: AdmittedBindings::default(),
            data: None,
            error: None,
        }
    }

    /// Bind one immutable `$config` field (§13.1).
    #[must_use]
    pub fn config(mut self, field: impl Into<String>, value: Value) -> Self {
        self.bindings.config.insert(field.into(), value);
        self
    }

    /// Supply the installation `$data` overlay (§13.3), as the JSON text of the
    /// `$data` object (`{"templates": {"a2": {"label": "Zeta"}}}`). Applied onto the
    /// child genesis after the package `$data` seed; every resulting value passes
    /// ordinary insertion and load validation.
    #[must_use]
    pub fn data(mut self, data: impl Into<String>) -> Self {
        self.data = Some(data.into());
        self
    }

    /// Declare a required `$use` handle bound to `spec` (§13.5). A malformed spec is
    /// remembered and surfaces when the host admits the request.
    #[must_use]
    pub fn use_handle(self, handle: impl Into<String>, spec: &str) -> Self {
        self.bind_use(handle, spec, false)
    }

    /// Declare an optional `$use.$optional` handle bound to `spec` (§13.5): its
    /// absence is valid and its private data is retained while unbound.
    #[must_use]
    pub fn optional_use(self, handle: impl Into<String>, spec: &str) -> Self {
        self.bind_use(handle, spec, true)
    }

    /// Declare a private `$deps` requirement (§13.6).
    #[must_use]
    pub fn dep(mut self, handle: impl Into<String>, spec: &str) -> Self {
        match DepSpec::parse(spec) {
            Ok(dep) => self.bindings.deps.push((handle.into(), dep)),
            Err(error) => {
                self.error.get_or_insert(error);
            }
        }
        self
    }

    fn bind_use(mut self, handle: impl Into<String>, spec: &str, optional: bool) -> Self {
        match UseSpec::parse(spec) {
            Ok(parsed) => self.bindings.uses.push((handle.into(), parsed, optional)),
            Err(error) => {
                self.error.get_or_insert(error);
            }
        }
        self
    }

    /// The instance name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The child package definition to load.
    #[must_use]
    pub fn definition(&self) -> &str {
        &self.definition
    }

    /// Consume the request into its recorded bindings and installation `$data`, or
    /// the first malformed-spec error, and validate the instance name is non-empty
    /// (§13.3).
    pub(crate) fn admit(self) -> Result<AdmittedInstall, ModuleError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.name.is_empty() {
            return Err(ModuleError::EmptyName);
        }
        Ok(AdmittedInstall {
            name: self.name,
            definition: self.definition,
            bindings: self.bindings,
            data: self.data,
        })
    }
}

/// An admitted install request (§13.3): the instance name, the child definition,
/// the boundary bindings, and the installation `$data` overlay text.
pub(crate) struct AdmittedInstall {
    pub(crate) name: String,
    pub(crate) definition: String,
    pub(crate) bindings: AdmittedBindings,
    pub(crate) data: Option<String>,
}
