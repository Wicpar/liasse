//! `liasse-blob-fs`: a filesystem [`BlobConnector`] (§18).
//!
//! [`FsConnector`] stores blob bytes as content-addressed objects on local disk
//! (§18.9): the path is derived from the blob's 64-byte SHA-512, so identical
//! content deduplicates and copies are idempotent, and the application filename
//! is never used for placement. Uploads are staged and committed with an atomic
//! rename (§18.7), so an interrupted upload never leaves a half-verified object.
//!
//! # Integrity (§18.9)
//!
//! The connector applies the canonical [`BlobIntegrity`] SHA-512 check twice:
//! on **ingress**, the bytes handed to [`upload`](FsConnector::upload) must hash
//! to the digest they are addressed by, and on **egress**, a whole-object
//! [`fetch`](FsConnector::fetch) re-hashes what it read before returning — a
//! corrupt or bit-rotted object fails loud rather than delivering wrong bytes.
//! A hash covers the exact byte sequence, so this subsumes a byte-count check.
//! Ranged reads ([`fetch_range`](FsConnector::fetch_range)) are the raw,
//! resumable transfer primitive of §18.8: a single range cannot be verified
//! against the whole-object digest, so the caller assembling the whole object
//! verifies it (this matches [`BlobIntegrity::fetch_range`]).
//!
//! # Capabilities (§18.12)
//!
//! [`FsConnector`] advertises exactly what a local filesystem can honestly
//! honour: streamed upload/download, ranged reads, server-side copy (a local
//! filesystem copy of content-addressed bytes is idempotent), checksum, delete,
//! and physical-usage observation. It deliberately does **not** advertise
//! presigned upload/download: a local filesystem has no URL a remote client can
//! use directly, so the runtime performs the transfer server-side (§18.8).
//!
//! # Registration
//!
//! ```no_run
//! use std::sync::Arc;
//! use liasse_blob_fs::FsConnector;
//! use liasse_host::Registry;
//!
//! let mut registry = Registry::new();
//! registry.register_connector("fs", Box::new(FsConnector::new("/var/lib/liasse/blobs")));
//! # let _keep = Arc::new(0);
//! ```
//!
//! The [`Registry`](liasse_host::Registry) holds connectors as
//! `Box<dyn BlobConnector>`; the SPEC §18.12 `context.blob_connector("fs",
//! Arc::new(...))` line is illustrative Rust for that same registration.

mod store;

use std::path::PathBuf;

use liasse_host::{
    BlobConnector, BlobIntegrity, ByteRange, Capability, ConnectorCapabilities, ConnectorFailure,
    UsageObservation,
};
use liasse_value::Sha512;

use crate::store::ContentStore;

/// A filesystem [`BlobConnector`] storing content-addressed blob objects under a
/// root directory (§18).
pub struct FsConnector {
    store: ContentStore,
}

impl FsConnector {
    /// A connector rooted at `root`. The directory tree is created lazily on the
    /// first upload, so a fresh or not-yet-existing root needs no provisioning.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            store: ContentStore::new(root.into()),
        }
    }

    /// The capabilities a local filesystem honestly honours (§18.12): everything
    /// but presigned upload/download, which a local filesystem cannot offer.
    #[must_use]
    pub fn advertised_capabilities() -> ConnectorCapabilities {
        ConnectorCapabilities::new([
            Capability::StreamUpload,
            Capability::StreamDownload,
            Capability::RangeReads,
            Capability::ServerSideCopy,
            Capability::Checksum,
            Capability::Delete,
            Capability::PhysicalUsage,
        ])
    }

    /// Verify that `bytes` hash to `digest`, mapping a mismatch to a typed
    /// connector failure (§18.9).
    fn verify(digest: &Sha512, bytes: &[u8]) -> Result<(), ConnectorFailure> {
        BlobIntegrity::new(*digest)
            .verify(bytes)
            .map_err(|mismatch| ConnectorFailure::Failed(mismatch.to_string()))
    }
}

impl BlobConnector for FsConnector {
    fn capabilities(&self) -> ConnectorCapabilities {
        Self::advertised_capabilities()
    }

    fn upload(&mut self, digest: &Sha512, bytes: &[u8]) -> Result<(), ConnectorFailure> {
        // Ingress verification (§18.9): the bytes must hash to the digest they
        // are addressed by, or nothing is committed.
        Self::verify(digest, bytes)?;
        self.store.commit(digest, bytes)
    }

    fn fetch(&self, digest: &Sha512) -> Result<Vec<u8>, ConnectorFailure> {
        let bytes = self.store.read(digest)?;
        // Egress verification (§18.9): never deliver bytes that no longer hash to
        // the digest — a corrupt object fails loud instead of returning wrong
        // bytes.
        Self::verify(digest, &bytes)?;
        Ok(bytes)
    }

    fn fetch_range(&self, digest: &Sha512, range: ByteRange) -> Result<Vec<u8>, ConnectorFailure> {
        self.store.read_range(digest, range)
    }

    fn exists(&self, digest: &Sha512) -> Result<bool, ConnectorFailure> {
        self.store.exists(digest)
    }

    fn delete(&mut self, digest: &Sha512) -> Result<(), ConnectorFailure> {
        self.store.remove(digest)
    }

    fn observe_usage(&self) -> Result<UsageObservation, ConnectorFailure> {
        self.store.usage()
    }
}
