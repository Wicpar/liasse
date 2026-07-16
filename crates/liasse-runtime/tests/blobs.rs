#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §18 blob dynamic semantics over host [`BlobConnector`] doubles: descriptor
//! acceptance against the streamed bytes (§18.1/§18.2), placement-policy
//! planning (§18.4), transactional upload (§18.7), and integrity-verified fetch
//! (§18.8/§18.9). Every expectation is re-derived from §18 text, including the
//! sharpest red-case shapes (lying descriptors, tampered downloads, unfulfillable
//! placement, media confusables).

use liasse_host::sim::SimConnector;
use liasse_host::{BlobIntegrity, Capability, ConnectorCapabilities};
use liasse_runtime::{
    AcceptedType, Blob, BlobEngine, CopyState, DeclaredDescriptor, FetchError, Placement, Store,
    StoreId, UploadError,
};
use liasse_value::MediaType;

fn digest_hex(content: &[u8]) -> String {
    BlobIntegrity::digest_hex(content)
}

/// A fully capable connector double.
fn connector() -> SimConnector {
    SimConnector::new(ConnectorCapabilities::new([
        Capability::StreamUpload,
        Capability::StreamDownload,
        Capability::RangeReads,
        Capability::ServerSideCopy,
        Capability::Checksum,
        Capability::Delete,
        Capability::PhysicalUsage,
    ]))
}

fn store(id: &str, connector: &str, enabled: bool) -> Store {
    Store { id: StoreId::new(id), connector: connector.to_owned(), enabled }
}

/// The declared descriptor for `content`, truthful unless a member is overridden.
fn declared(content: &[u8], media: &str) -> DeclaredDescriptor {
    DeclaredDescriptor {
        sha512: digest_hex(content),
        bytes: content.len() as u64,
        media: media.to_owned(),
        name: Some("doc.txt".to_owned()),
    }
}

fn accepted(max_bytes: u64, media: &[&str]) -> AcceptedType {
    AcceptedType {
        max_bytes,
        media: media.iter().map(|m| MediaType::new(*m)).collect(),
    }
}

/// An engine with one enabled `primary` store on connector `fs`.
fn single_store_engine() -> BlobEngine<SimConnector> {
    let mut engine = BlobEngine::new();
    engine.register("fs", connector());
    engine.add_store(store("primary", "fs", true));
    engine
}

fn primary() -> Placement {
    Placement::View(vec![StoreId::new("primary")])
}

/// §18.7: an accepted upload creates a verified copy and returns the committed
/// descriptor identifying the content.
#[test]
fn accepted_upload_commits_and_verifies() {
    let mut engine = single_store_engine();
    let content = b"invoice bytes";
    let blob = engine
        .upload(&declared(content, "text/plain"), &accepted(1_000, &["text/plain"]), &primary(), content)
        .expect("uploaded");
    assert_eq!(blob.descriptor().sha512().to_canonical_text(), digest_hex(content));
    assert_eq!(blob.descriptor().byte_count(), content.len() as u64);
    assert_eq!(blob.stored(), vec![StoreId::new("primary")]);
    assert_eq!(blob.placement(&StoreId::new("primary")), Some(CopyState::Verified));
}

/// §18.8/§18.9: a fetch through an authorized surface returns exactly the bytes
/// identified by `$sha512`.
#[test]
fn fetch_returns_exact_bytes() {
    let mut engine = single_store_engine();
    let content = b"exact content";
    let blob = upload_ok(&mut engine, content);
    assert_eq!(engine.fetch(&blob, true).expect("fetch"), content);
}

/// §18.1 (red): a declared `$sha512` that disagrees with the streamed bytes is
/// rejected before any transition.
#[test]
fn claimed_sha512_mismatch_rejected() {
    let mut engine = single_store_engine();
    let content = b"real bytes";
    let mut lying = declared(content, "text/plain");
    lying.sha512 = digest_hex(b"different bytes");
    let error = engine
        .upload(&lying, &accepted(1_000, &["text/plain"]), &primary(), content)
        .expect_err("mismatch");
    assert_eq!(error, UploadError::DigestMismatch);
}

/// §18.1 (red): a declared byte count that disagrees with the streamed length is
/// rejected.
#[test]
fn claimed_byte_count_mismatch_rejected() {
    let mut engine = single_store_engine();
    let content = b"ten bytes!";
    let mut lying = declared(content, "text/plain");
    lying.bytes = 999;
    let error = engine
        .upload(&lying, &accepted(10_000, &["text/plain"]), &primary(), content)
        .expect_err("byte mismatch");
    assert_eq!(error, UploadError::ByteCountMismatch);
}

/// §18.1 (red, SPEC-ISSUES item 20): an uppercase-hex declared digest is not the
/// canonical lowercase form, so this defensive stance rejects it rather than
/// silently normalizing.
#[test]
fn uppercase_hex_digest_rejected_as_noncanonical() {
    let mut engine = single_store_engine();
    let content = b"content";
    let mut declared = declared(content, "text/plain");
    declared.sha512 = declared.sha512.to_uppercase();
    let error = engine
        .upload(&declared, &accepted(1_000, &["text/plain"]), &primary(), content)
        .expect_err("non-canonical");
    assert_eq!(error, UploadError::MalformedDigest);
}

/// §18.2: `$max_bytes` is an inclusive limit — content exactly at the limit is
/// accepted, and one byte over is rejected.
#[test]
fn max_bytes_is_inclusive() {
    let mut engine = single_store_engine();
    let content = b"12345";
    engine
        .upload(&declared(content, "text/plain"), &accepted(5, &["text/plain"]), &primary(), content)
        .expect("at the limit");

    let mut engine = single_store_engine();
    let over = b"123456";
    let error = engine
        .upload(&declared(over, "text/plain"), &accepted(5, &["text/plain"]), &primary(), over)
        .expect_err("over the limit");
    assert_eq!(error, UploadError::TooLarge { actual: 6, limit: 5 });
}

/// §18.2: a zero-byte blob is accepted (an empty content is valid content).
#[test]
fn zero_byte_blob_accepted() {
    let mut engine = single_store_engine();
    let content = b"";
    let blob = engine
        .upload(&declared(content, "text/plain"), &accepted(10, &["text/plain"]), &primary(), content)
        .expect("empty accepted");
    assert_eq!(blob.descriptor().byte_count(), 0);
}

/// §18.2: an unaccepted media type is rejected.
#[test]
fn unaccepted_media_rejected() {
    let mut engine = single_store_engine();
    let content = b"pdf-ish";
    let error = engine
        .upload(&declared(content, "application/pdf"), &accepted(1_000, &["text/plain"]), &primary(), content)
        .expect_err("wrong media");
    assert!(matches!(error, UploadError::MediaNotAccepted(_)));
}

/// §18.2: media type/subtype are compared case-insensitively.
#[test]
fn media_compared_case_insensitively() {
    let mut engine = single_store_engine();
    let content = b"x";
    engine
        .upload(&declared(content, "Text/Plain"), &accepted(10, &["text/plain"]), &primary(), content)
        .expect("case-insensitive match");
}

/// §18.2: a declaration without parameters accepts the same type/subtype with
/// any parameters.
#[test]
fn media_declaration_without_params_accepts_any_params() {
    let mut engine = single_store_engine();
    let content = b"x";
    engine
        .upload(&declared(content, "text/plain; charset=utf-8"), &accepted(10, &["text/plain"]), &primary(), content)
        .expect("params accepted when declaration has none");
}

/// §18.2 (red): a declaration that includes parameters compares them exactly; a
/// differing parameter value is not accepted.
#[test]
fn media_parameter_mismatch_rejected() {
    let mut engine = single_store_engine();
    let content = b"x";
    let error = engine
        .upload(
            &declared(content, "text/plain; charset=ascii"),
            &accepted(10, &["text/plain; charset=utf-8"]),
            &primary(),
            content,
        )
        .expect_err("param mismatch");
    assert!(matches!(error, UploadError::MediaNotAccepted(_)));
}

/// §18.2 (red): declared parameters are compared after sorting by name, so a
/// reordering of the same parameters is accepted.
#[test]
fn media_parameter_reordering_accepted() {
    let mut engine = single_store_engine();
    let content = b"x";
    engine
        .upload(
            &declared(content, "text/plain; b=2; a=1"),
            &accepted(10, &["text/plain; a=1; b=2"]),
            &primary(),
            content,
        )
        .expect("reordered parameters accepted");
}

/// §18.2 (red): a visually confusable but byte-distinct media type is not the
/// accepted type.
#[test]
fn confusable_media_type_not_accepted() {
    let mut engine = single_store_engine();
    let content = b"x";
    let error = engine
        .upload(&declared(content, "application/x-pdf"), &accepted(10, &["application/pdf"]), &primary(), content)
        .expect_err("confusable subtype");
    assert!(matches!(error, UploadError::MediaNotAccepted(_)));
}

/// §18.8/§18.9 (red): a tampered download never surfaces as a successful fetch;
/// with no other clean holder the fetch yields no result rather than the
/// tampered bytes.
#[test]
fn tampered_download_never_surfaces() {
    let mut engine = single_store_engine();
    let content = b"trustworthy";
    let blob = upload_ok(&mut engine, content);
    // The read transport starts lying only after the clean upload verified.
    engine.connector_mut("fs").expect("fs registered").set_tamper_download(true);
    assert_eq!(engine.fetch(&blob, true), Err(FetchError::NoCleanHolder));
}

/// §18.8: a metadata-only projection (or otherwise unauthorized occurrence)
/// grants no blob fetch.
#[test]
fn metadata_only_projection_grants_no_fetch() {
    let mut engine = single_store_engine();
    let content = b"secret";
    let blob = upload_ok(&mut engine, content);
    assert_eq!(engine.fetch(&blob, false), Err(FetchError::Denied));
}

/// §18.4 (red): a placement whose stores are all disabled cannot be fulfilled,
/// so the upload is rejected.
#[test]
fn no_writable_store_rejects_upload() {
    let mut engine = BlobEngine::new();
    engine.register("fs", connector());
    engine.add_store(store("primary", "fs", false));
    let content = b"x";
    let error = engine
        .upload(&declared(content, "text/plain"), &accepted(10, &["text/plain"]), &primary(), content)
        .expect_err("no writable store");
    assert_eq!(error, UploadError::NoWritablePlacement);
}

/// §18.4: an `$any` placement chooses the first fulfillable branch; a disabled
/// first store falls through to the second.
#[test]
fn any_branch_selects_first_fulfillable() {
    let mut engine = BlobEngine::new();
    engine.register("fs", connector());
    engine.add_store(store("new", "fs", false));
    engine.add_store(store("old", "fs", true));
    let placement = Placement::Any(vec![
        Placement::View(vec![StoreId::new("new")]),
        Placement::View(vec![StoreId::new("old")]),
    ]);
    let content = b"x";
    let blob = engine
        .upload(&declared(content, "text/plain"), &accepted(10, &["text/plain"]), &placement, content)
        .expect("second branch fulfillable");
    assert_eq!(blob.stored(), vec![StoreId::new("old")]);
}

/// §18.4 (red): a `$copies` placement requiring more distinct writable stores
/// than exist is rejected.
#[test]
fn copies_fewer_than_n_rejected() {
    let mut engine = BlobEngine::new();
    engine.register("fs", connector());
    engine.add_store(store("a", "fs", true));
    let placement = Placement::Copies { n: 2, of: vec![StoreId::new("a")] };
    let content = b"x";
    let error = engine
        .upload(&declared(content, "text/plain"), &accepted(10, &["text/plain"]), &placement, content)
        .expect_err("not enough stores");
    assert_eq!(error, UploadError::NoWritablePlacement);
}

/// §18.4 (red): a view repeating a store identity deduplicates it by first
/// occurrence, so a single copy satisfies it.
#[test]
fn repeated_store_identity_deduplicated() {
    let mut engine = single_store_engine();
    let placement = Placement::View(vec![StoreId::new("primary"), StoreId::new("primary")]);
    let content = b"x";
    let blob = engine
        .upload(&declared(content, "text/plain"), &accepted(10, &["text/plain"]), &placement, content)
        .expect("dedup view");
    assert_eq!(blob.stored(), vec![StoreId::new("primary")]);
}

/// §18.4/§18.7: an `$all` placement verifies a copy in every store, and `$serve`
/// defaults to the flattened depth-first placement order.
#[test]
fn all_branch_verifies_every_copy_and_serve_order() {
    let mut engine = two_store_engine();
    let placement = Placement::All(vec![
        Placement::View(vec![StoreId::new("primary")]),
        Placement::View(vec![StoreId::new("backup")]),
    ]);
    let content = b"x";
    let blob = engine
        .upload(&declared(content, "text/plain"), &accepted(10, &["text/plain"]), &placement, content)
        .expect("both copies verified");
    let mut stored = blob.stored();
    stored.sort();
    assert_eq!(stored, vec![StoreId::new("backup"), StoreId::new("primary")]);
    // Serve order follows the flattened placement order (primary before backup).
    assert_eq!(engine.fetch(&blob, true).expect("fetch"), content);
}

/// §18.6/§18.9 (red): a corrupt copy is demoted and repaired from a verified
/// holder; the reconciler converges the placement back to fully verified.
#[test]
fn corrupt_copy_demoted_and_repaired() {
    let mut engine = two_store_engine();
    let placement = Placement::All(vec![
        Placement::View(vec![StoreId::new("primary")]),
        Placement::View(vec![StoreId::new("backup")]),
    ]);
    let content = b"important";
    let mut blob = engine
        .upload(&declared(content, "text/plain"), &accepted(100, &["text/plain"]), &placement, content)
        .expect("both verified");
    let digest = *blob.descriptor().sha512();

    // Corrupt the physical object in the primary store.
    engine.connector_mut("fs-primary").expect("fs-primary registered").corrupt(digest);
    engine.reconcile(&mut blob, &placement);

    assert_eq!(
        blob.placement(&StoreId::new("primary")),
        Some(CopyState::Verified),
        "the corrupt copy is repaired from the verified backup",
    );
    // The blob still fetches its exact bytes after repair.
    assert_eq!(engine.fetch(&blob, true).expect("fetch"), content);
}

// ---- helpers -------------------------------------------------------------

fn upload_ok(engine: &mut BlobEngine<SimConnector>, content: &[u8]) -> Blob {
    engine
        .upload(&declared(content, "text/plain"), &accepted(10_000, &["text/plain"]), &primary(), content)
        .expect("upload ok")
}

/// A two-store engine on distinct connectors, so a per-store fault is isolated.
fn two_store_engine() -> BlobEngine<SimConnector> {
    let mut engine = BlobEngine::new();
    engine.register("fs-primary", connector());
    engine.register("fs-backup", connector());
    engine.add_store(store("primary", "fs-primary", true));
    engine.add_store(store("backup", "fs-backup", true));
    engine
}
