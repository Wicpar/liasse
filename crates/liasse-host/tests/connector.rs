//! Blob-connector double behaviour observed through the [`BlobConnector`]
//! contract, and the §18.9 fetch-verification guarantee: a well-behaved
//! round-trip; ranged reads; capability checks (§18.12); clean failures and
//! unavailability; and the negative guarantee that a tampered/corrupt read is
//! never delivered as a successful verified fetch.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use liasse_host::sim::ConnectorOp;
use liasse_host::{
    BlobConnector, BlobIntegrity, ByteRange, Capability, ConnectorFailure, VerifiedFetchError,
};

use common::fs_connector;

fn digest_of(bytes: &[u8]) -> liasse_value::Sha512 {
    liasse_value::Sha512::parse(&BlobIntegrity::digest_hex(bytes)).expect("digest parses")
}

/// A well-behaved upload/fetch round-trip verifies clean and returns the exact
/// bytes (§18.9).
#[test]
fn verified_round_trip() {
    let content = b"hello world";
    let digest = digest_of(content);
    let mut connector = fs_connector();
    connector.upload(&digest, content).expect("uploaded");

    let integrity = BlobIntegrity::new(digest);
    let fetched = integrity.fetch_verified(&connector).expect("verified fetch");
    assert_eq!(fetched, content);
}

/// A ranged read returns the requested slice (§18.12 range reads).
#[test]
fn ranged_read() {
    let content = b"0123456789";
    let digest = digest_of(content);
    let mut connector = fs_connector();
    connector.upload(&digest, content).expect("uploaded");

    let range = ByteRange::new(2, 5).expect("valid range");
    let slice = connector.fetch_range(&digest, range).expect("range fetch");
    assert_eq!(slice, b"234");
}

/// A tampered download (transport lie) never surfaces as a successful verified
/// fetch: the hash mismatch is caught before delivery (§18.8/§18.9). The stored
/// object is unchanged, so the connector still reports it present.
#[test]
fn tampered_download_never_delivered() {
    let content = b"trustworthy";
    let digest = digest_of(content);
    let mut connector = fs_connector();
    connector.upload(&digest, content).expect("uploaded");
    connector.set_tamper_download(true);

    let integrity = BlobIntegrity::new(digest);
    match integrity.fetch_verified(&connector) {
        Err(VerifiedFetchError::Tampered(_)) => {}
        other => panic!("expected Tampered, got {other:?}"),
    }
    assert!(connector.exists(&digest).expect("exists check"));
}

/// A corrupt stored object also fails verification on the next read (§18.9).
#[test]
fn corrupt_object_fails_verification() {
    let content = b"payload";
    let digest = digest_of(content);
    let mut connector = fs_connector();
    connector.upload(&digest, content).expect("uploaded");
    connector.corrupt(digest);

    let integrity = BlobIntegrity::new(digest);
    assert!(matches!(
        integrity.fetch_verified(&connector),
        Err(VerifiedFetchError::Tampered(_))
    ));
}

/// A scripted upload failure rejects cleanly, storing nothing (§18.12).
#[test]
fn scripted_upload_failure_stores_nothing() {
    let content = b"data";
    let digest = digest_of(content);
    let mut connector = fs_connector();
    connector.set_fail([ConnectorOp::Upload]);
    assert!(matches!(
        connector.upload(&digest, content),
        Err(ConnectorFailure::Failed(_))
    ));
    assert!(!connector.exists(&digest).expect("exists check"));
}

/// An unavailable connector fails every operation (§18.12).
#[test]
fn unavailable_connector_fails_every_op() {
    let digest = digest_of(b"x");
    let mut connector = fs_connector();
    connector.set_available(false);
    assert!(matches!(
        connector.fetch(&digest),
        Err(ConnectorFailure::Unavailable)
    ));
    assert!(matches!(
        connector.observe_usage(),
        Err(ConnectorFailure::Unavailable)
    ));
}

/// §18.12 capability checks: a required capability the connector lacks is a
/// typed shortfall.
#[test]
fn capability_check_reports_shortfall() {
    let connector = fs_connector();
    let capabilities = connector.capabilities();
    capabilities
        .satisfies([Capability::RangeReads, Capability::Delete])
        .expect("satisfied");

    let minimal = liasse_host::ConnectorCapabilities::new([Capability::StreamUpload]);
    assert!(minimal.satisfies([Capability::RangeReads]).is_err());
}

/// Connector-reported usage reflects the stored objects (§18.11).
#[test]
fn observed_usage_reflects_stored_objects() {
    let content = b"observe-me";
    let digest = digest_of(content);
    let mut connector = fs_connector();
    connector.upload(&digest, content).expect("uploaded");

    let usage = connector.observe_usage().expect("usage");
    assert_eq!(usage.object_count, 1);
    assert_eq!(usage.physical_bytes, content.len() as u64);
}
