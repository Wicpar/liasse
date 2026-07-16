//! The [`BlobConnector`] contract (§18.12): one external storage system behind
//! a typed interface. The logical model keeps identity, placement, integrity,
//! and billing observable; the connector performs physical transfer.
//!
//! Object-safe and synchronous. Operations that change the backing store
//! (`upload`, `delete`) take `&mut self`; reads take `&self`. A temporary
//! connector failure rejects or delays the affected operation while committed
//! application state is preserved (§18.12).

use std::collections::BTreeSet;

use liasse_value::Sha512;

/// A capability a connector advertises (§18.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// Streamed upload.
    StreamUpload,
    /// Streamed download.
    StreamDownload,
    /// Presigned upload URL.
    PresignedUpload,
    /// Presigned download URL.
    PresignedDownload,
    /// Ranged reads.
    RangeReads,
    /// Server-side copy between objects.
    ServerSideCopy,
    /// Checksum support.
    Checksum,
    /// Object deletion.
    Delete,
    /// Physical-usage observation.
    PhysicalUsage,
}

/// The capability set a connector advertises for the §18.12 load-time checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorCapabilities(BTreeSet<Capability>);

impl ConnectorCapabilities {
    /// Assemble a capability set.
    #[must_use]
    pub fn new(capabilities: impl IntoIterator<Item = Capability>) -> Self {
        Self(capabilities.into_iter().collect())
    }

    /// Whether the connector advertises `capability`.
    #[must_use]
    pub fn has(&self, capability: Capability) -> bool {
        self.0.contains(&capability)
    }

    /// Check these capabilities against those a declared placement/fetch
    /// behaviour requires (§18.12). Returns the first missing capability, or
    /// `Ok(())` when every requirement is met.
    pub fn satisfies(
        &self,
        required: impl IntoIterator<Item = Capability>,
    ) -> Result<(), CapabilityShortfall> {
        for capability in required {
            if !self.has(capability) {
                return Err(CapabilityShortfall(capability));
            }
        }
        Ok(())
    }
}

/// A capability a declared placement/fetch behaviour requires that the
/// connector does not advertise (§18.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("connector does not advertise required capability `{0:?}`")]
pub struct CapabilityShortfall(pub Capability);

/// A half-open byte range for a ranged read (§18.12 range reads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    start: u64,
    end: u64,
}

impl ByteRange {
    /// Build a `[start, end)` range, or `None` if `end < start`.
    #[must_use]
    pub const fn new(start: u64, end: u64) -> Option<Self> {
        if end < start {
            None
        } else {
            Some(Self { start, end })
        }
    }

    /// The inclusive start offset.
    #[must_use]
    pub const fn start(self) -> u64 {
        self.start
    }

    /// The exclusive end offset.
    #[must_use]
    pub const fn end(self) -> u64 {
        self.end
    }

    /// The number of bytes the range spans.
    #[must_use]
    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    /// Whether the range is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.end == self.start
    }
}

/// A connector-reported physical-usage observation (§18.11). It is a host
/// observation for reconciliation/audit; it becomes application data only
/// through an explicit committed observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageObservation {
    /// Number of objects the connector reports holding.
    pub object_count: u64,
    /// Total physical bytes the connector reports.
    pub physical_bytes: u64,
}

/// A registered blob connector (§18.12).
///
/// Uploads are content-addressed by SHA-512 (§18.9). A successful `fetch` must
/// deliver exactly the bytes identified by the requested digest; the runtime
/// verifies this before delivery (§18.9), so a connector cannot make tampered
/// bytes a successful fetch — see [`crate::BlobIntegrity`].
pub trait BlobConnector {
    /// The capabilities this connector advertises (§18.12).
    fn capabilities(&self) -> ConnectorCapabilities;

    /// Upload `bytes` content-addressed by `digest`. Idempotent: storing the
    /// same content twice is one object.
    fn upload(&mut self, digest: &Sha512, bytes: &[u8]) -> Result<(), ConnectorFailure>;

    /// Fetch the full object for `digest`. The returned bytes are unverified at
    /// this boundary — a compromised connector MAY return mismatching content.
    fn fetch(&self, digest: &Sha512) -> Result<Vec<u8>, ConnectorFailure>;

    /// Fetch a byte range of the object for `digest` (§18.12 range reads).
    fn fetch_range(
        &self,
        digest: &Sha512,
        range: ByteRange,
    ) -> Result<Vec<u8>, ConnectorFailure>;

    /// Whether the connector holds an object for `digest`.
    fn exists(&self, digest: &Sha512) -> Result<bool, ConnectorFailure>;

    /// Delete the object for `digest`.
    fn delete(&mut self, digest: &Sha512) -> Result<(), ConnectorFailure>;

    /// The connector's reported physical usage (§18.11).
    fn observe_usage(&self) -> Result<UsageObservation, ConnectorFailure>;
}

/// A typed connector failure (§18.12). A rejected or delayed operation leaves
/// committed application state unchanged and produces no partial verified copy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectorFailure {
    /// No object is held for the requested digest.
    #[error("no object held for digest")]
    NotFound,
    /// A requested range lay outside the object's bytes.
    #[error("range [{start}, {end}) is outside the object's {len} bytes")]
    RangeOutOfBounds {
        /// Requested start.
        start: u64,
        /// Requested end.
        end: u64,
        /// Object length.
        len: u64,
    },
    /// The connector does not advertise the capability the operation needs.
    #[error("operation requires unavailable capability `{0:?}`")]
    Unsupported(Capability),
    /// The connector is unavailable and can perform no operation (§18.12).
    #[error("connector is unavailable")]
    Unavailable,
    /// A clean, deterministic operation failure the double injects (§18.12).
    #[error("connector operation failed: {0}")]
    Failed(String),
}
