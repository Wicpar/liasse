//! Durable identity (Annex D.1 / D.5).
//!
//! Rows and package instances carry an opaque immutable *incarnation* allocated
//! at insertion / installation. Rekeying a row or renaming/rebinding an instance
//! preserves that incarnation; delete-then-insert (or uninstall-then-install)
//! allocates a new one (D.1). History is addressed by lineage and point (D.5).
//!
//! An incarnation is opaque: the annex pins no internal structure, so the
//! canonical textual form of each identity is the opaque token itself, and
//! equality is token equality.

use crate::path::CanonicalPath;

/// Generate an opaque immutable identity newtype. The token is carried verbatim;
/// its canonical text is itself.
macro_rules! opaque_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Wrap an opaque token allocated by the runtime.
            #[must_use]
            pub fn new(token: impl Into<String>) -> Self {
                Self(token.into())
            }

            /// The opaque token.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// The canonical textual form — the opaque token itself.
            #[must_use]
            pub fn to_canonical_text(&self) -> String {
                self.0.clone()
            }

            /// Consume into the owned token.
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

opaque_id! {
    /// A package instance's immutable incarnation (D.1).
    InstanceId
}
opaque_id! {
    /// A collection row's immutable incarnation, allocated at insertion (D.1).
    RowIncarnation
}
opaque_id! {
    /// A history lineage identifier (D.5).
    LineageId
}
opaque_id! {
    /// A history point identifier within a lineage (D.5).
    PointId
}
opaque_id! {
    /// A committing transaction identifier (D.5).
    TransactionId
}
opaque_id! {
    /// A compacted history-archive range identifier (D.5).
    RangeId
}

/// A row's durable identity: its current application address plus its immutable
/// incarnation (D.1).
///
/// Per D.1 the durable identity **is** the incarnation, so equality, ordering,
/// and hashing are by incarnation alone; the address is mutable provenance. This
/// makes the mandated invariants hold directly: an atomic rekey keeps the
/// incarnation, so the rekeyed identity still equals the original; a
/// delete-then-insert gets a fresh incarnation, so distinct lineages differ.
#[derive(Debug, Clone)]
pub struct RowIdentity {
    address: CanonicalPath,
    incarnation: RowIncarnation,
}

impl RowIdentity {
    /// Bind an address to a freshly allocated (or looked-up) incarnation.
    #[must_use]
    pub fn new(address: CanonicalPath, incarnation: RowIncarnation) -> Self {
        Self {
            address,
            incarnation,
        }
    }

    /// The current application address (changes on rekey).
    #[must_use]
    pub fn address(&self) -> &CanonicalPath {
        &self.address
    }

    /// The immutable incarnation — the durable identity.
    #[must_use]
    pub fn incarnation(&self) -> &RowIncarnation {
        &self.incarnation
    }

    /// Apply an atomic rekey (§5.4): the address changes, the incarnation and
    /// therefore the durable identity are preserved.
    #[must_use]
    pub fn rekey(self, new_address: CanonicalPath) -> Self {
        Self {
            address: new_address,
            incarnation: self.incarnation,
        }
    }
}

impl PartialEq for RowIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.incarnation == other.incarnation
    }
}
impl Eq for RowIdentity {}
impl core::hash::Hash for RowIdentity {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.incarnation.hash(state);
    }
}

/// A package instance's durable identity: its current mount label plus its
/// immutable incarnation (D.1). Renaming or rebinding preserves the incarnation,
/// so equality is by incarnation alone, mirroring [`RowIdentity`].
#[derive(Debug, Clone)]
pub struct InstanceIdentity {
    label: String,
    incarnation: InstanceId,
}

impl InstanceIdentity {
    /// Bind a mount label to an instance incarnation.
    #[must_use]
    pub fn new(label: impl Into<String>, incarnation: InstanceId) -> Self {
        Self {
            label: label.into(),
            incarnation,
        }
    }

    /// The current mount label (changes on rename/rebind).
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The immutable incarnation — the durable identity.
    #[must_use]
    pub fn incarnation(&self) -> &InstanceId {
        &self.incarnation
    }

    /// Rename or rebind the instance (D.1): the label changes, the incarnation
    /// and therefore the durable identity are preserved.
    #[must_use]
    pub fn rebind(self, new_label: impl Into<String>) -> Self {
        Self {
            label: new_label.into(),
            incarnation: self.incarnation,
        }
    }
}

impl PartialEq for InstanceIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.incarnation == other.incarnation
    }
}
impl Eq for InstanceIdentity {}
impl core::hash::Hash for InstanceIdentity {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.incarnation.hash(state);
    }
}

/// A history point's full identity: its lineage and its point (D.5).
///
/// A bare [`PointId`] is not a cross-lineage identity — the same point token may
/// recur in unrelated lineages (SPEC-ISSUES item 21, point-id aliasing). Pairing
/// it with its [`LineageId`] keeps points from unrelated histories distinct, so
/// equality and ordering span both.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HistoryPoint {
    lineage: LineageId,
    point: PointId,
}

impl HistoryPoint {
    /// Identify a point within its lineage.
    #[must_use]
    pub fn new(lineage: LineageId, point: PointId) -> Self {
        Self { lineage, point }
    }

    /// The lineage.
    #[must_use]
    pub fn lineage(&self) -> &LineageId {
        &self.lineage
    }

    /// The point within the lineage.
    #[must_use]
    pub fn point(&self) -> &PointId {
        &self.point
    }
}
