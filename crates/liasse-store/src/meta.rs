//! Durable per-instance metadata: the active definition and the current
//! composition of mounted children (§19.1, §19.5, Annex D.4).

use std::collections::BTreeMap;

use liasse_ident::{DefinitionId, HistoryPoint, InstanceId};

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
}

impl Mount {
    /// Bind a child incarnation to a selected point.
    #[must_use]
    pub fn new(instance: InstanceId, selected: HistoryPoint) -> Self {
        Self { instance, selected }
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
