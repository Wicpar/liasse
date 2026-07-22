//! §18.8 `$serve` read order — proven at the runtime blob API with a
//! read-recording [`BlobConnector`] double.
//!
//! §18.4: "`$serve` controls preferred read order and defaults to that flattened
//! placement order." §18.8: a runtime fetch "MUST attempt each accessible
//! verified holder in `$serve` order". So with `$in = $all[primary, archive]`
//! (flattening to `[primary, archive]`) and `$serve = [archive]`, the fetch plan
//! puts `archive` first and the runtime reads `archive` before `primary` — the
//! reverse of the default.
//!
//! Externally deducible: the recording connectors log which store each `fetch`
//! reads from. A fetch returns on the first hash-clean holder, so the log after
//! one fetch names exactly the first holder the plan attempted. With no `$serve`
//! the first holder is `primary` (flattened order); with `$serve = [archive]` it
//! is `archive`. The full `blob.serve_order()` plan is asserted alongside.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use liasse_host::{
    BlobConnector, BlobIntegrity, ByteRange, Capability, ConnectorCapabilities, ConnectorFailure,
    UsageObservation,
};
use liasse_runtime::{
    AcceptedType, BlobEngine, DeclaredDescriptor, Placement, PlacementPolicy, StoreId,
};
use liasse_value::{MediaType, Sha512};

/// A [`BlobConnector`] double that appends its store `label` to a shared,
/// ordered read log on every `fetch`. The log is shared across the engine-owned
/// connectors (an `Arc<Mutex<…>>`, the only way to observe *cross-connector*
/// read ORDER — a per-connector counter cannot order two connectors), which is
/// legitimate for a test observing the runtime's fetch sequence, not smuggled
/// mutability into engine state.
struct RecordingConnector {
    label: String,
    objects: BTreeMap<Sha512, Vec<u8>>,
    reads: Arc<Mutex<Vec<String>>>,
}

impl RecordingConnector {
    fn new(label: &str, reads: Arc<Mutex<Vec<String>>>) -> Self {
        Self { label: label.to_owned(), objects: BTreeMap::new(), reads }
    }
}

impl BlobConnector for RecordingConnector {
    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities::new([Capability::StreamUpload, Capability::StreamDownload, Capability::Checksum])
    }

    fn upload(&mut self, digest: &Sha512, bytes: &[u8]) -> Result<(), ConnectorFailure> {
        self.objects.insert(*digest, bytes.to_vec());
        Ok(())
    }

    fn fetch(&self, digest: &Sha512) -> Result<Vec<u8>, ConnectorFailure> {
        self.reads.lock().expect("read log").push(self.label.clone());
        self.objects.get(digest).cloned().ok_or(ConnectorFailure::NotFound)
    }

    fn fetch_range(&self, digest: &Sha512, range: ByteRange) -> Result<Vec<u8>, ConnectorFailure> {
        self.reads.lock().expect("read log").push(self.label.clone());
        let bytes = self.objects.get(digest).ok_or(ConnectorFailure::NotFound)?;
        let len = bytes.len() as u64;
        if range.end() > len {
            return Err(ConnectorFailure::RangeOutOfBounds { start: range.start(), end: range.end(), len });
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
        Ok(UsageObservation {
            object_count: self.objects.len() as u64,
            physical_bytes: self.objects.values().map(|v| v.len() as u64).sum(),
        })
    }
}

const CONTENT: &[u8] = b"served bytes";

fn accepted() -> AcceptedType {
    AcceptedType { max_bytes: 1_000, media: vec![MediaType::new("text/plain")] }
}

fn declared() -> DeclaredDescriptor {
    DeclaredDescriptor {
        sha512: BlobIntegrity::digest_hex(CONTENT),
        bytes: CONTENT.len() as u64,
        media: "text/plain".to_owned(),
        name: None,
    }
}

/// A two-store engine over recording connectors sharing `reads`. `$in` is
/// `$all[primary, archive]`, so both hold a verified copy; the flattened
/// placement order is `[primary, archive]`.
fn engine(reads: &Arc<Mutex<Vec<String>>>) -> BlobEngine<RecordingConnector> {
    let mut engine = BlobEngine::new();
    engine.register("conn-primary", RecordingConnector::new("primary", Arc::clone(reads)));
    engine.register("conn-archive", RecordingConnector::new("archive", Arc::clone(reads)));
    engine.add_store(liasse_runtime::Store {
        id: StoreId::new("primary"),
        connector: "conn-primary".to_owned(),
        enabled: true,
    });
    engine.add_store(liasse_runtime::Store {
        id: StoreId::new("archive"),
        connector: "conn-archive".to_owned(),
        enabled: true,
    });
    engine
}

fn in_plan() -> Placement {
    Placement::All(vec![
        Placement::View(vec![StoreId::new("primary")]),
        Placement::View(vec![StoreId::new("archive")]),
    ])
}

/// CONTROL — no `$serve`: the serve order defaults to the flattened placement
/// order `[primary, archive]`, so the runtime reads `primary` first (§18.4).
#[test]
fn default_serve_order_reads_flattened_first() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let mut engine = engine(&reads);
    let policy: PlacementPolicy = in_plan().into();

    let blob = engine.upload(&declared(), &accepted(), &policy, CONTENT).expect("upload");
    assert_eq!(
        blob.serve_order(),
        [StoreId::new("primary"), StoreId::new("archive")],
        "default serve order is the flattened placement order",
    );

    reads.lock().unwrap().clear(); // discard the §18.9 upload-verification reads
    assert_eq!(engine.fetch(&blob, true).expect("fetch"), CONTENT);
    assert_eq!(
        *reads.lock().unwrap(),
        vec!["primary".to_owned()],
        "with no $serve the runtime reads the flattened-first holder (primary)",
    );
}

/// PROBE — `$serve = [archive]`: §18.4 makes `archive` the preferred read order
/// and §18.8 makes the runtime attempt holders in that order, so it reads
/// `archive` FIRST even though `primary` is first in the `$in` flattening.
#[test]
fn serve_order_reads_declared_store_first() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let mut engine = engine(&reads);
    let policy = PlacementPolicy::new(in_plan(), Some(vec![StoreId::new("archive")]));

    let blob = engine.upload(&declared(), &accepted(), &policy, CONTENT).expect("upload");
    assert_eq!(
        blob.serve_order(),
        [StoreId::new("archive"), StoreId::new("primary")],
        "$serve = [archive] puts archive first, then the remaining flattened holder",
    );

    reads.lock().unwrap().clear(); // discard the §18.9 upload-verification reads
    assert_eq!(engine.fetch(&blob, true).expect("fetch"), CONTENT);
    assert_eq!(
        *reads.lock().unwrap(),
        vec!["archive".to_owned()],
        "$serve reorders the fetch plan: the runtime reads archive before primary (§18.8)",
    );
}
