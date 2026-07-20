#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §18.5/§18.6/§18.12 red team: reconcile must demote a verified blob copy to
//! `corrupt` ONLY on a genuine hash-mismatch, never because a connector is
//! temporarily unreachable.
//!
//! §18.5 defines `corrupt` as "observed to hash wrong". §18.6 says "A corrupt
//! observation demotes that copy and triggers repair from another verified
//! holder." §18.12: "Temporary connector failure rejects or delays the affected
//! operation while preserving committed application state." A verified placement
//! row is committed application state (§18.5 "logical observations recorded by
//! the engine"). So:
//!
//! - a transient transport outage — which delivers NO bytes and therefore cannot
//!   "hash wrong" — must NOT demote a verified copy, and must leave a sole copy
//!   recoverable once the connector returns (`temporary_outage_*`); while
//! - a genuinely tampered copy (bytes delivered that hash wrong) MUST still
//!   demote and be repaired from another verified holder (`genuinely_tampered_*`),
//!   so the fix does not weaken the §18.6 repair path.

use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::BTreeMap;

use liasse_host::{
    BlobConnector, BlobIntegrity, ByteRange, Capability, ConnectorCapabilities, ConnectorFailure,
    UsageObservation,
};
use liasse_runtime::{
    AcceptedType, Blob, BlobEngine, CopyState, DeclaredDescriptor, Placement, Store, StoreId,
};
use liasse_value::{MediaType, Sha512};

/// A content-addressed connector with a runtime `available` toggle and a
/// `tamper` fault. When unavailable, every operation fails with
/// `ConnectorFailure::Unavailable` — a TEMPORARY transport outage (§18.12), not
/// byte corruption. `tamper` replaces a stored object's bytes with a sequence
/// whose SHA-512 no longer matches the descriptor — the §18.9 corrupt object.
struct ToggleConnector {
    objects: BTreeMap<Sha512, Vec<u8>>,
    available: AtomicBool,
}

impl ToggleConnector {
    fn new() -> Self {
        Self { objects: BTreeMap::new(), available: AtomicBool::new(true) }
    }

    /// Corrupt the physical bytes held for `digest` (a bit-rot / tamper): the
    /// object now hashes wrong, so a verified fetch observes `Tampered`.
    fn tamper(&mut self, digest: &Sha512) {
        self.objects.insert(*digest, b"tampered payload".to_vec());
    }
}

impl BlobConnector for ToggleConnector {
    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities::new([
            Capability::StreamUpload,
            Capability::StreamDownload,
            Capability::Delete,
        ])
    }

    fn upload(&mut self, digest: &Sha512, bytes: &[u8]) -> Result<(), ConnectorFailure> {
        if !self.available.load(Ordering::Relaxed) {
            return Err(ConnectorFailure::Unavailable);
        }
        self.objects.insert(*digest, bytes.to_vec());
        Ok(())
    }

    fn fetch(&self, digest: &Sha512) -> Result<Vec<u8>, ConnectorFailure> {
        if !self.available.load(Ordering::Relaxed) {
            // Temporary outage: no bytes delivered. The object is NOT corrupt.
            return Err(ConnectorFailure::Unavailable);
        }
        self.objects.get(digest).cloned().ok_or(ConnectorFailure::NotFound)
    }

    fn fetch_range(&self, digest: &Sha512, range: ByteRange) -> Result<Vec<u8>, ConnectorFailure> {
        let bytes = self.fetch(digest)?;
        let len = bytes.len() as u64;
        if range.end() > len {
            return Err(ConnectorFailure::RangeOutOfBounds { start: range.start(), end: range.end(), len });
        }
        Ok(bytes[range.start() as usize..range.end() as usize].to_vec())
    }

    fn exists(&self, digest: &Sha512) -> Result<bool, ConnectorFailure> {
        if !self.available.load(Ordering::Relaxed) {
            return Err(ConnectorFailure::Unavailable);
        }
        Ok(self.objects.contains_key(digest))
    }

    fn delete(&mut self, digest: &Sha512) -> Result<(), ConnectorFailure> {
        if !self.available.load(Ordering::Relaxed) {
            return Err(ConnectorFailure::Unavailable);
        }
        self.objects.remove(digest);
        Ok(())
    }

    fn observe_usage(&self) -> Result<UsageObservation, ConnectorFailure> {
        Err(ConnectorFailure::Unsupported(Capability::PhysicalUsage))
    }
}

const CONTENT: &[u8] = b"the verified copy bytes";

fn declared() -> DeclaredDescriptor {
    DeclaredDescriptor {
        sha512: BlobIntegrity::digest_hex(CONTENT),
        bytes: CONTENT.len() as u64,
        media: "text/plain".to_owned(),
        name: None,
    }
}

fn accepted() -> AcceptedType {
    AcceptedType { max_bytes: 10_000, media: vec![MediaType::new("text/plain")] }
}

fn digest() -> Sha512 {
    Sha512::parse(&BlobIntegrity::digest_hex(CONTENT)).expect("digest")
}

/// One store `s1` on connector `c`, holding a single verified copy.
fn engine_with_one_copy() -> (BlobEngine<ToggleConnector>, Blob, Placement) {
    let mut engine = BlobEngine::new();
    engine.register("c", ToggleConnector::new());
    engine.add_store(Store { id: StoreId::new("s1"), connector: "c".to_owned(), enabled: true });

    let placement = Placement::View(vec![StoreId::new("s1")]);
    let blob = engine
        .upload(&declared(), &accepted(), &placement, CONTENT)
        .expect("upload lands one verified copy in s1");
    assert_eq!(blob.placement(&StoreId::new("s1")), Some(CopyState::Verified));
    (engine, blob, placement)
}

/// §18.12/§18.5: reconcile while the sole holder's connector is temporarily
/// unavailable must preserve the verified copy — a transient outage is not a
/// `corrupt` observation.
#[test]
fn temporary_outage_does_not_demote_verified_copy() {
    let (mut engine, mut blob, placement) = engine_with_one_copy();

    engine.connector_mut("c").expect("connector").available.store(false, Ordering::Relaxed);
    engine.reconcile(&mut blob, &placement);

    let state = blob.placement(&StoreId::new("s1"));
    assert_eq!(
        state,
        Some(CopyState::Verified),
        "§18.12/§18.5: a temporary connector outage must not demote a verified copy to \
         `corrupt` (got {state:?})",
    );
}

/// A copy left untouched by a transient outage is recoverable: once the
/// connector returns, the sole clean copy is verified again (never stuck
/// `corrupt` with no repair source).
#[test]
fn sole_copy_survives_outage_and_recovery() {
    let (mut engine, mut blob, placement) = engine_with_one_copy();

    engine.connector_mut("c").expect("connector").available.store(false, Ordering::Relaxed);
    engine.reconcile(&mut blob, &placement);
    engine.connector_mut("c").expect("connector").available.store(true, Ordering::Relaxed);
    engine.reconcile(&mut blob, &placement);

    assert_eq!(
        blob.stored(),
        vec![StoreId::new("s1")],
        "after the connector recovers the clean copy in s1 must remain verified, got {:?}",
        blob.stored(),
    );
}

/// §18.6: a GENUINELY tampered copy (bytes that hash wrong) must still be
/// demoted and repaired from another verified holder — the fix narrows the
/// demotion trigger to real corruption, it does not remove it.
#[test]
fn genuinely_tampered_copy_is_demoted_and_repaired() {
    let mut engine = BlobEngine::new();
    engine.register("c1", ToggleConnector::new());
    engine.register("c2", ToggleConnector::new());
    engine.add_store(Store { id: StoreId::new("s1"), connector: "c1".to_owned(), enabled: true });
    engine.add_store(Store { id: StoreId::new("s2"), connector: "c2".to_owned(), enabled: true });

    // Both stores hold a verified copy (`$all` requires both).
    let placement = Placement::All(vec![
        Placement::View(vec![StoreId::new("s1")]),
        Placement::View(vec![StoreId::new("s2")]),
    ]);
    let mut blob = engine
        .upload(&declared(), &accepted(), &placement, CONTENT)
        .expect("upload lands verified copies in s1 and s2");
    assert_eq!(blob.placement(&StoreId::new("s1")), Some(CopyState::Verified));
    assert_eq!(blob.placement(&StoreId::new("s2")), Some(CopyState::Verified));

    // s1's physical object is corrupted; s2 stays clean.
    engine.connector_mut("c1").expect("c1").tamper(&digest());

    engine.reconcile(&mut blob, &placement);

    // §18.6: the corrupt observation demoted s1 and repaired it from s2, so both
    // copies are verified again.
    assert_eq!(
        blob.stored(),
        vec![StoreId::new("s1"), StoreId::new("s2")],
        "a genuinely tampered copy must demote and repair from the clean holder, got {:?}",
        blob.stored(),
    );
}
