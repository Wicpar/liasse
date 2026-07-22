#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §18 red team: a store whose connector advertises the WRITE capabilities and
//! is fully reachable — but does not support the *optional* physical-usage
//! observation — must be a valid placement target for a new write.
//!
//! Spec basis (all externally deducible, no tautology):
//! - §18.4: a new write "chooses the first `n` currently writable capable
//!   stores"; writability/capability is about the store's ability to hold the
//!   write, i.e. the upload capability (§18.2 "every verified copy required by
//!   one complete branch of the current placement plan").
//! - §18.11: "Connector-reported physical usage is a host observation for
//!   reconciliation and auditing. It becomes application data only through an
//!   explicit committed observation." Physical usage is auxiliary, not a write
//!   prerequisite.
//! - §18.12: the connector contract lists "physical usage observation" as one
//!   OPTIONAL advertised capability among many (`delete`, `range reads`, ...);
//!   "Loading validates connector capabilities required by declared placement
//!   and client behavior" — placement into a store requires the write
//!   capability, not usage observation.
//!
//! A connector may legitimately not advertise `PhysicalUsage` and therefore
//! return `Unsupported(PhysicalUsage)` from `observe_usage` (the documented
//! meaning of `Unsupported`). Such a store, when it can `upload` + verify, MUST
//! still accept a new write.
//!
//! Actual (bug): `BlobEngine::writable` (crates/liasse-runtime/src/blobs.rs)
//! gates writability on `observe_usage().is_ok()`, conflating the optional
//! usage-observation capability with write-liveness, so the upload is rejected
//! `NoWritablePlacement` even though the connector fully performs and verifies
//! the copy.

use std::collections::BTreeMap;

use liasse_host::{
    BlobConnector, BlobIntegrity, ByteRange, Capability, ConnectorCapabilities, ConnectorFailure,
    UsageObservation,
};
use liasse_runtime::{
    AcceptedType, BlobEngine, DeclaredDescriptor, Placement, PlacementPolicy, Store, StoreId,
    UploadError,
};
use liasse_value::{MediaType, Sha512};

/// A fully functional content-addressed connector that can upload, download,
/// range-read, and delete — but does NOT advertise `PhysicalUsage` and so
/// returns `Unsupported(PhysicalUsage)` from `observe_usage` (§18.11/§18.12: a
/// legitimate connector without a usage API). Every write/read operation works.
#[derive(Default)]
struct WriteCapableNoUsageConnector {
    objects: BTreeMap<Sha512, Vec<u8>>,
}

impl BlobConnector for WriteCapableNoUsageConnector {
    fn capabilities(&self) -> ConnectorCapabilities {
        // Advertises the write + read + delete capabilities a placement target
        // needs; deliberately omits `PhysicalUsage`.
        ConnectorCapabilities::new([
            Capability::StreamUpload,
            Capability::StreamDownload,
            Capability::RangeReads,
            Capability::Delete,
        ])
    }

    fn upload(&mut self, digest: &Sha512, bytes: &[u8]) -> Result<(), ConnectorFailure> {
        self.objects.insert(*digest, bytes.to_vec());
        Ok(())
    }

    fn fetch(&self, digest: &Sha512) -> Result<Vec<u8>, ConnectorFailure> {
        self.objects.get(digest).cloned().ok_or(ConnectorFailure::NotFound)
    }

    fn fetch_range(
        &self,
        digest: &Sha512,
        range: ByteRange,
    ) -> Result<Vec<u8>, ConnectorFailure> {
        let bytes = self.fetch(digest)?;
        let len = bytes.len() as u64;
        if range.end() > len {
            return Err(ConnectorFailure::RangeOutOfBounds {
                start: range.start(),
                end: range.end(),
                len,
            });
        }
        Ok(bytes[range.start() as usize..range.end() as usize].to_vec())
    }

    fn exists(&self, digest: &Sha512) -> Result<bool, ConnectorFailure> {
        Ok(self.objects.contains_key(digest))
    }

    fn delete(&mut self, digest: &Sha512) -> Result<(), ConnectorFailure> {
        self.objects.remove(digest);
        Ok(())
    }

    fn observe_usage(&self) -> Result<UsageObservation, ConnectorFailure> {
        // The connector does not advertise `PhysicalUsage`; asking it to report
        // physical usage is `Unsupported` — NOT a liveness/write failure.
        Err(ConnectorFailure::Unsupported(Capability::PhysicalUsage))
    }
}

/// §18.4/§18.11/§18.12: a store whose connector can upload+verify a copy is a
/// valid new-write placement target even though it does not support the optional
/// physical-usage observation.
#[test]
fn upload_accepts_store_whose_connector_lacks_usage_observation() {
    let mut engine = BlobEngine::new();
    engine.register("append", WriteCapableNoUsageConnector::default());
    engine.add_store(Store {
        id: StoreId::new("primary"),
        connector: "append".to_owned(),
        enabled: true,
    });

    let content = b"invoice bytes";
    let declared = DeclaredDescriptor {
        sha512: BlobIntegrity::digest_hex(content),
        bytes: content.len() as u64,
        media: "text/plain".to_owned(),
        name: None,
    };
    let accepted = AcceptedType {
        max_bytes: 1_000,
        media: vec![MediaType::new("text/plain")],
    };
    let placement: PlacementPolicy = Placement::View(vec![StoreId::new("primary")]).into();

    let result = engine.upload(&declared, &accepted, &placement, content);

    // Per §18.4/§18.11/§18.12 the connector is upload-capable and reachable, so
    // the write must commit a verified copy in `primary`.
    match result {
        Ok(blob) => {
            assert_eq!(blob.stored(), vec![StoreId::new("primary")]);
        }
        Err(UploadError::NoWritablePlacement) => panic!(
            "store wrongly excluded from placement: its connector advertises \
             StreamUpload and fully performs+verifies the copy, but \
             BlobEngine::writable requires observe_usage().is_ok(), conflating \
             the OPTIONAL PhysicalUsage capability (§18.11/§18.12) with \
             write-liveness (§18.4)"
        ),
        Err(other) => panic!("unexpected rejection: {other:?}"),
    }
}
