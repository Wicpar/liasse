//! Durable per-instance metadata: the active definition and the current
//! composition of mounted children (§19.1, §19.5, Annex D.4).

use std::collections::BTreeMap;

use liasse_ident::{DefinitionId, HistoryPoint, InstanceId};
use liasse_value::Sha512;

/// The decoded-package provenance a §13.10 lifecycle op pins on a mount (§5.1): the
/// artifact/content id of the `.liasse` blob it decoded, the D.4 definition identity,
/// and the `major.minor.patch` package version. Recorded as a fact of the commit that
/// mounts or migrates the instance, so audit and replay report WHICH package bytes are
/// in force, never re-derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagePin {
    content: Sha512,
    definition: DefinitionId,
    version: [u64; 3],
}

impl PackagePin {
    /// Pin the decoded package: its blob content id, D.4 definition id, and version.
    #[must_use]
    pub fn new(content: Sha512, definition: DefinitionId, version: [u64; 3]) -> Self {
        Self { content, definition, version }
    }

    /// The content id (SHA-512) of the `.liasse` blob the package was decoded from.
    #[must_use]
    pub fn content(&self) -> &Sha512 {
        &self.content
    }

    /// The D.4 definition identity of the decoded `liasse.json`.
    #[must_use]
    pub fn definition(&self) -> &DefinitionId {
        &self.definition
    }

    /// The `major.minor.patch` package version.
    #[must_use]
    pub fn version(&self) -> [u64; 3] {
        self.version
    }
}

/// The definition text active for a package instance, with its canonical
/// identity (D.4).
///
/// The store keeps the `liasse.json` source verbatim and its SHA-256 definition
/// identifier together: the identifier is what a manifest and a composition
/// point reference, and pairing them means the store can serve either without
/// re-hashing. Two texts with the same identity are the same definition (D.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionText {
    identity: DefinitionId,
    source: String,
}

impl DefinitionText {
    /// Wrap definition source, computing its canonical identity from the bytes.
    #[must_use]
    pub fn new(source: impl Into<String>) -> Self {
        let source = source.into();
        let identity = DefinitionId::of_canonical_bytes(source.as_bytes());
        Self { identity, source }
    }

    /// The canonical definition identifier (D.4).
    #[must_use]
    pub fn identity(&self) -> &DefinitionId {
        &self.identity
    }

    /// The `liasse.json` source text.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// One mounted child instance in a composition: its incarnation and the history
/// point selected for it (§19.5 `manifest.modules`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    instance: InstanceId,
    selected: HistoryPoint,
    /// The decoded-package provenance a §13.10 lifecycle op pinned on this mount
    /// (§5.1), or `None` for an instance mounted through the ordinary install path
    /// (no blob decode).
    package: Option<PackagePin>,
}

impl Mount {
    /// Bind a child incarnation to a selected point, with no package provenance.
    #[must_use]
    pub fn new(instance: InstanceId, selected: HistoryPoint) -> Self {
        Self { instance, selected, package: None }
    }

    /// Bind a child incarnation to a selected point WITH the decoded-package
    /// provenance a §13.10 lifecycle op recorded (§5.1).
    #[must_use]
    pub fn pinned(instance: InstanceId, selected: HistoryPoint, package: PackagePin) -> Self {
        Self { instance, selected, package: Some(package) }
    }

    /// The child instance incarnation.
    #[must_use]
    pub fn instance(&self) -> &InstanceId {
        &self.instance
    }

    /// The selected child history point.
    #[must_use]
    pub fn selected(&self) -> &HistoryPoint {
        &self.selected
    }

    /// The decoded-package provenance pinned on this mount (§5.1), if any.
    #[must_use]
    pub fn package(&self) -> Option<&PackagePin> {
        self.package.as_ref()
    }
}

/// The current composition of one parent instance: its direct child mounts keyed
/// by mount name (§19.3 composition point, §19.5).
///
/// A `BTreeMap` keeps mount names in a stable order — the store records the
/// selection; which children are legal to mount is the runtime's concern.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composition {
    mounts: BTreeMap<String, Mount>,
}

impl Composition {
    /// An empty composition — no children mounted.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the mount at `name`, replacing any prior selection there.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, mount: Mount) -> Self {
        self.mounts.insert(name.into(), mount);
        self
    }

    /// The mount selected at `name`, if any.
    #[must_use]
    pub fn mount(&self, name: &str) -> Option<&Mount> {
        self.mounts.get(name)
    }

    /// The mounts in mount-name order.
    pub fn mounts(&self) -> impl Iterator<Item = (&str, &Mount)> {
        self.mounts.iter().map(|(name, mount)| (name.as_str(), mount))
    }
}
