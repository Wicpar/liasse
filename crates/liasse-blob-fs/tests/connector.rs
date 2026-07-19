//! [`FsConnector`] behaviour observed through the [`BlobConnector`] contract and
//! the filesystem-specific §18 guarantees: a content-addressed round-trip that
//! verifies clean (§18.9); deduplication and idempotent copying (§18.9); ranged
//! and resumable reads that reconstruct the exact bytes (§18.8); delete;
//! tamper/corruption detection that never delivers wrong bytes (§18.9);
//! transactional staging that commits no half-verified object (§18.7); the
//! honest capability set (§18.12); and connector-reported usage (§18.11).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use liasse_blob_fs::FsConnector;
use liasse_host::{
    BlobConnector, BlobIntegrity, ByteRange, Capability, ConnectorFailure, VerifiedFetchError,
};
use liasse_value::Sha512;
use tempfile::TempDir;

/// The canonical SHA-512 of `bytes`, as the descriptor pins it (§18.1).
fn digest_of(bytes: &[u8]) -> Sha512 {
    Sha512::parse(&BlobIntegrity::digest_hex(bytes)).expect("digest parses")
}

/// The content-addressed path bytes must occupy under `root` (§18.9).
fn content_path(root: &Path, bytes: &[u8]) -> PathBuf {
    let hex = BlobIntegrity::digest_hex(bytes);
    let first = hex.get(0..2).expect("shard 1");
    let second = hex.get(2..4).expect("shard 2");
    root.join(first).join(second).join(&hex)
}

/// A connector over a throwaway temporary root, kept alive with it.
fn connector() -> (FsConnector, TempDir) {
    let root = tempfile::tempdir().expect("temp root");
    let connector = FsConnector::new(root.path());
    (connector, root)
}

/// A well-behaved upload lands the bytes at the content-addressed path and a
/// verified fetch returns exactly them (§18.9).
#[test]
fn verified_round_trip_is_content_addressed() {
    let content = b"hello world";
    let digest = digest_of(content);
    let (mut connector, root) = connector();

    connector.upload(&digest, content).expect("uploaded");

    // The object is stored by hash, never by any application filename (§18.9).
    let expected = content_path(root.path(), content);
    assert!(expected.is_file(), "object must live at its content path");

    let integrity = BlobIntegrity::new(digest);
    let fetched = integrity.fetch_verified(&connector).expect("verified fetch");
    assert_eq!(fetched, content);
}

/// Identical content addressed twice is one stored object — deduplicated and
/// idempotent to copy (§18.9).
#[test]
fn identical_content_deduplicates() {
    let content = b"shared bytes";
    let digest = digest_of(content);
    let (mut connector, root) = connector();

    // Two descriptors naming the same content resolve to the same digest.
    connector.upload(&digest, content).expect("first upload");
    connector.upload(&digest, content).expect("idempotent re-upload");

    let usage = connector.observe_usage().expect("usage");
    assert_eq!(usage.object_count, 1, "same content is one object");
    assert_eq!(usage.physical_bytes, content.len() as u64);
    assert!(content_path(root.path(), content).is_file());
}

/// A ranged read returns the requested slice, and consecutive ranges reconstruct
/// the exact bytes — resumable reads (§18.8/§18.12).
#[test]
fn ranged_and_resumable_reads_reconstruct_bytes() {
    let content = b"0123456789";
    let digest = digest_of(content);
    let (mut connector, _root) = connector();
    connector.upload(&digest, content).expect("uploaded");

    let middle = ByteRange::new(2, 5).expect("valid range");
    assert_eq!(connector.fetch_range(&digest, middle).expect("range"), b"234");

    // Resume across two windows and reassemble the whole object, then verify the
    // reconstruction hashes back to the descriptor (§18.8).
    let head = connector
        .fetch_range(&digest, ByteRange::new(0, 4).expect("range"))
        .expect("head");
    let tail = connector
        .fetch_range(&digest, ByteRange::new(4, 10).expect("range"))
        .expect("tail");
    let mut assembled = head;
    assembled.extend_from_slice(&tail);
    assert_eq!(assembled, content);
    BlobIntegrity::new(digest)
        .verify(&assembled)
        .expect("reassembled bytes verify");
}

/// A range past the object's bytes is a typed out-of-bounds failure (§18.12).
#[test]
fn range_past_end_is_out_of_bounds() {
    let content = b"short";
    let digest = digest_of(content);
    let (mut connector, _root) = connector();
    connector.upload(&digest, content).expect("uploaded");

    let past = ByteRange::new(3, 99).expect("valid range");
    assert!(matches!(
        connector.fetch_range(&digest, past),
        Err(ConnectorFailure::RangeOutOfBounds { len: 5, .. })
    ));
}

/// Delete removes the object and is idempotent on a missing one (§18.12).
#[test]
fn delete_removes_and_is_idempotent() {
    let content = b"disposable";
    let digest = digest_of(content);
    let (mut connector, root) = connector();
    connector.upload(&digest, content).expect("uploaded");
    assert!(content_path(root.path(), content).is_file());

    connector.delete(&digest).expect("deleted");
    assert!(!connector.exists(&digest).expect("exists check"));
    assert!(!content_path(root.path(), content).exists());
    // Deleting again is a no-op, not a failure.
    connector.delete(&digest).expect("idempotent delete");
    assert!(matches!(connector.fetch(&digest), Err(ConnectorFailure::NotFound)));
}

/// A corrupted on-disk object is caught by hash verification and never returned
/// as bytes; the tamper is same-length, so only the SHA-512 catches it (§18.9).
#[test]
fn corrupt_object_fails_loud_never_returns_wrong_bytes() {
    let content = b"trustworthy!";
    let digest = digest_of(content);
    let (mut connector, root) = connector();
    connector.upload(&digest, content).expect("uploaded");

    // Overwrite the stored object with different bytes of the same length.
    let path = content_path(root.path(), content);
    let tampered = b"tampered!!!!";
    assert_eq!(tampered.len(), content.len());
    fs::write(&path, tampered).expect("tamper on disk");

    // A direct fetch fails loud rather than returning the wrong bytes.
    match connector.fetch(&digest) {
        Err(ConnectorFailure::Failed(_)) => {}
        other => panic!("expected a loud integrity failure, got {other:?}"),
    }
    // And through the runtime's verified fetch, tampered bytes never come back Ok.
    let integrity = BlobIntegrity::new(digest);
    assert!(matches!(
        integrity.fetch_verified(&connector),
        Err(VerifiedFetchError::Connector(ConnectorFailure::Failed(_)))
    ));
    // The object is still physically present — a demotable corrupt copy (§18.5).
    assert!(connector.exists(&digest).expect("exists check"));
}

/// Bytes that do not hash to their claimed digest are refused on ingress, and
/// nothing is committed (§18.9).
#[test]
fn ingress_hash_mismatch_commits_nothing() {
    let claimed = digest_of(b"the real content");
    let (mut connector, root) = connector();

    assert!(matches!(
        connector.upload(&claimed, b"a different payload"),
        Err(ConnectorFailure::Failed(_))
    ));
    assert!(!connector.exists(&claimed).expect("exists check"));
    assert!(!content_path(root.path(), b"the real content").exists());
    assert!(!content_path(root.path(), b"a different payload").exists());
}

/// An interrupted upload's staged object is not committed state: it is never
/// counted and never served (§18.7). A completed upload commits exactly one
/// object alongside it.
#[test]
fn staged_leftovers_are_not_committed_state() {
    let (mut connector, root) = connector();

    // Simulate an interrupted upload's leftover: a stray file under the staging
    // directory, outside the content tree.
    let staging = root.path().join(".staging");
    fs::create_dir_all(&staging).expect("staging dir");
    fs::write(staging.join("tmp-partial-upload"), b"half-written bytes").expect("stray");

    assert_eq!(
        connector.observe_usage().expect("usage").object_count,
        0,
        "a staged leftover is not a committed object"
    );

    let content = b"committed after";
    let digest = digest_of(content);
    connector.upload(&digest, content).expect("uploaded");

    let usage = connector.observe_usage().expect("usage");
    assert_eq!(usage.object_count, 1, "only the committed object counts");
    assert_eq!(usage.physical_bytes, content.len() as u64);
}

/// The advertised capability set is exactly what a local filesystem can honestly
/// honour: everything but presigned upload/download (§18.12).
#[test]
fn capabilities_are_the_honest_filesystem_set() {
    let (connector, _root) = connector();
    let capabilities = connector.capabilities();

    for capability in [
        Capability::StreamUpload,
        Capability::StreamDownload,
        Capability::RangeReads,
        Capability::ServerSideCopy,
        Capability::Checksum,
        Capability::Delete,
        Capability::PhysicalUsage,
    ] {
        assert!(capabilities.has(capability), "must advertise {capability:?}");
    }
    // A local filesystem has no presign; the runtime fetches server-side (§18.8).
    assert!(!capabilities.has(Capability::PresignedUpload));
    assert!(!capabilities.has(Capability::PresignedDownload));

    // A placement routed to `fs` requiring these capabilities passes §18.6/§18.12.
    capabilities
        .satisfies([Capability::RangeReads, Capability::Delete, Capability::ServerSideCopy])
        .expect("filesystem placement is satisfiable");
    assert!(capabilities.satisfies([Capability::PresignedDownload]).is_err());
}

/// Connector-reported usage reflects the committed objects and their sizes
/// (§18.11).
#[test]
fn observed_usage_reflects_stored_objects() {
    let (mut connector, _root) = connector();
    let first = b"one";
    let second = b"the second, longer";
    connector.upload(&digest_of(first), first).expect("upload one");
    connector.upload(&digest_of(second), second).expect("upload two");

    let usage = connector.observe_usage().expect("usage");
    assert_eq!(usage.object_count, 2);
    assert_eq!(usage.physical_bytes, (first.len() + second.len()) as u64);
}

/// A fetch for content the store never held is a typed not-found (§18.12).
#[test]
fn fetch_missing_is_not_found() {
    let (connector, _root) = connector();
    let digest = digest_of(b"never uploaded");
    assert!(matches!(connector.fetch(&digest), Err(ConnectorFailure::NotFound)));
    assert!(!connector.exists(&digest).expect("exists check"));
}
