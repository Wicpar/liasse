#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §18 blob put/get over the surface [`BlobHost`] façade, driven against host
//! [`SimConnector`] doubles: a content upload commits and is fetchable by digest,
//! a metadata-only projection grants no fetch, a lying descriptor is rejected, a
//! tampered read transport never surfaces its bytes, and an unknown digest is
//! reported as such.

use liasse_host::sim::SimConnector;
use liasse_host::{BlobIntegrity, Capability, ConnectorCapabilities};
use liasse_surface::{
    AcceptedType, BlobEngine, BlobGetOutcome, BlobHost, BlobPutOutcome, DeclaredDescriptor,
    Placement, Store, StoreId,
};
use liasse_value::MediaType;

fn connector() -> SimConnector {
    SimConnector::new(ConnectorCapabilities::new([
        Capability::StreamUpload,
        Capability::StreamDownload,
        Capability::Checksum,
        Capability::Delete,
        Capability::PhysicalUsage,
    ]))
}

/// A blob host with one enabled `primary` store on connector `fs`, accepting up
/// to 1000 bytes of `text/plain`, placed in `primary`.
fn blob_host() -> BlobHost<SimConnector> {
    let mut engine = BlobEngine::new();
    engine.register("fs", connector());
    engine.add_store(Store { id: StoreId::new("primary"), connector: "fs".to_owned(), enabled: true });
    let accepted = AcceptedType { max_bytes: 1_000, media: vec![MediaType::new("text/plain")] };
    BlobHost::new(engine, accepted, Placement::View(vec![StoreId::new("primary")]))
}

/// §18.7/§18.8: an accepted upload commits and the exact bytes are fetchable by
/// their digest through an authorized (visible) projection.
#[test]
fn put_then_get_round_trips_by_digest() {
    let mut host = blob_host();
    let content = b"invoice bytes";
    let BlobPutOutcome::Committed { digest, stored } = host.put(content, "text/plain") else {
        panic!("upload should commit");
    };
    assert_eq!(digest, BlobIntegrity::digest_hex(content));
    assert_eq!(stored, vec![StoreId::new("primary")]);

    assert_eq!(host.get(&digest, true), BlobGetOutcome::Delivered(content.to_vec()));
    assert_eq!(host.stored(&digest), Some(vec![StoreId::new("primary")]));
}

/// §18.8: a metadata-only projection (visibility gate `false`) grants no fetch.
#[test]
fn metadata_only_projection_denied() {
    let mut host = blob_host();
    let BlobPutOutcome::Committed { digest, .. } = host.put(b"secret", "text/plain") else {
        panic!("upload should commit");
    };
    assert_eq!(host.get(&digest, false), BlobGetOutcome::Denied);
}

/// §18.1 (red): a declared `$sha512` disagreeing with the streamed bytes is
/// rejected before any transition.
#[test]
fn lying_descriptor_rejected() {
    let mut host = blob_host();
    let content = b"real bytes";
    let lying = DeclaredDescriptor {
        sha512: BlobIntegrity::digest_hex(b"different"),
        bytes: content.len() as u64,
        media: "text/plain".to_owned(),
        name: None,
    };
    assert!(matches!(host.put_declared(&lying, content), BlobPutOutcome::Rejected(_)));
    // Nothing was retained under the (wrong) digest.
    assert_eq!(host.get(&lying.sha512, true), BlobGetOutcome::Unknown);
}

/// §18.8/§18.9 (red): a tampered read transport never surfaces as a successful
/// fetch; with no other clean holder the fetch reports no clean holder.
#[test]
fn tampered_download_never_surfaces() {
    let mut host = blob_host();
    let BlobPutOutcome::Committed { digest, .. } = host.put(b"trustworthy", "text/plain") else {
        panic!("upload should commit");
    };
    host.connector_mut("fs").expect("fs registered").set_tamper_download(true);
    assert_eq!(host.get(&digest, true), BlobGetOutcome::NoCleanHolder);
}

/// A digest that was never put is reported unknown rather than delivered or denied.
#[test]
fn unknown_digest_reported() {
    let host = blob_host();
    assert_eq!(host.get(&BlobIntegrity::digest_hex(b"never uploaded"), true), BlobGetOutcome::Unknown);
}
