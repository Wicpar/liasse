//! A scriptable blob-connector double covering the corpus `connector_set`
//! vocabulary (tests/18-blobs/NOTES.md, tests/23-host-contract/NOTES.md):
//! full unavailability (`available: false`), clean per-operation failures
//! (`fail`), stored-object corruption (`corrupt`, observed as a hash mismatch
//! on the next verification), and a lying read transport (`tamper_download`,
//! which returns mismatching bytes while the stored object stays intact).

use std::collections::{BTreeMap, BTreeSet};

use liasse_value::Sha512;

use crate::connector::{
    BlobConnector, ByteRange, ConnectorCapabilities, ConnectorFailure, UsageObservation,
};

/// A connector operation the double can be scripted to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConnectorOp {
    /// [`BlobConnector::upload`].
    Upload,
    /// [`BlobConnector::fetch`] / [`BlobConnector::fetch_range`].
    Download,
    /// A server-side copy (reserved; no dedicated trait method yet).
    Copy,
    /// [`BlobConnector::delete`].
    Delete,
}

/// A scriptable [`BlobConnector`] double.
pub struct SimConnector {
    capabilities: ConnectorCapabilities,
    objects: BTreeMap<Sha512, Vec<u8>>,
    available: bool,
    fail: BTreeSet<ConnectorOp>,
    corrupt: BTreeSet<Sha512>,
    tamper_download: bool,
}

impl SimConnector {
    /// Build a double advertising `capabilities`, holding no objects.
    #[must_use]
    pub fn new(capabilities: ConnectorCapabilities) -> Self {
        Self {
            capabilities,
            objects: BTreeMap::new(),
            available: true,
            fail: BTreeSet::new(),
            corrupt: BTreeSet::new(),
            tamper_download: false,
        }
    }

    /// Set whether the connector is available; when false every operation fails
    /// [`ConnectorFailure::Unavailable`] (§18.12).
    pub fn set_available(&mut self, available: bool) {
        self.available = available;
    }

    /// Script the operations that fail cleanly (`connector_set { fail }`).
    pub fn set_fail(&mut self, ops: impl IntoIterator<Item = ConnectorOp>) {
        self.fail = ops.into_iter().collect();
    }

    /// Corrupt the stored object for `digest` (`connector_set { corrupt }`):
    /// subsequent reads return bytes whose SHA-512 no longer matches, modelling
    /// a tampered/bit-rotted physical object.
    pub fn corrupt(&mut self, digest: Sha512) {
        self.corrupt.insert(digest);
    }

    /// Make read bytes mismatch their digest while the stored object stays
    /// intact (`connector_set { tamper_download }`): a lying/compromised read
    /// transport rather than observed bit-rot.
    pub fn set_tamper_download(&mut self, tamper: bool) {
        self.tamper_download = tamper;
    }

    fn gate(&self, op: ConnectorOp) -> Result<(), ConnectorFailure> {
        if !self.available {
            return Err(ConnectorFailure::Unavailable);
        }
        if self.fail.contains(&op) {
            return Err(ConnectorFailure::Failed(format!("injected failure for {op:?}")));
        }
        Ok(())
    }

    /// The bytes a read serves: the stored object, mangled when the object is
    /// corrupt or the transport is tampering so its hash no longer matches.
    fn served(&self, digest: &Sha512) -> Option<Vec<u8>> {
        let stored = self.objects.get(digest)?;
        let mut bytes = stored.clone();
        if self.corrupt.contains(digest) || self.tamper_download {
            bytes.extend_from_slice(b"-tampered");
        }
        Some(bytes)
    }
}

impl BlobConnector for SimConnector {
    fn capabilities(&self) -> ConnectorCapabilities {
        self.capabilities.clone()
    }

    fn upload(&mut self, digest: &Sha512, bytes: &[u8]) -> Result<(), ConnectorFailure> {
        self.gate(ConnectorOp::Upload)?;
        self.objects.insert(*digest, bytes.to_vec());
        Ok(())
    }

    fn fetch(&self, digest: &Sha512) -> Result<Vec<u8>, ConnectorFailure> {
        self.gate(ConnectorOp::Download)?;
        self.served(digest).ok_or(ConnectorFailure::NotFound)
    }

    fn fetch_range(
        &self,
        digest: &Sha512,
        range: ByteRange,
    ) -> Result<Vec<u8>, ConnectorFailure> {
        self.gate(ConnectorOp::Download)?;
        let bytes = self.served(digest).ok_or(ConnectorFailure::NotFound)?;
        let len = bytes.len() as u64;
        if range.end() > len {
            return Err(ConnectorFailure::RangeOutOfBounds {
                start: range.start(),
                end: range.end(),
                len,
            });
        }
        let slice = bytes
            .get(range.start() as usize..range.end() as usize)
            .ok_or(ConnectorFailure::RangeOutOfBounds {
                start: range.start(),
                end: range.end(),
                len,
            })?;
        Ok(slice.to_vec())
    }

    fn exists(&self, digest: &Sha512) -> Result<bool, ConnectorFailure> {
        if !self.available {
            return Err(ConnectorFailure::Unavailable);
        }
        Ok(self.objects.contains_key(digest))
    }

    fn delete(&mut self, digest: &Sha512) -> Result<(), ConnectorFailure> {
        self.gate(ConnectorOp::Delete)?;
        self.objects.remove(digest);
        self.corrupt.remove(digest);
        Ok(())
    }

    fn observe_usage(&self) -> Result<UsageObservation, ConnectorFailure> {
        if !self.available {
            return Err(ConnectorFailure::Unavailable);
        }
        let physical_bytes = self.objects.values().map(|v| v.len() as u64).sum();
        Ok(UsageObservation {
            object_count: self.objects.len() as u64,
            physical_bytes,
        })
    }
}
