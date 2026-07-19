//! Verification catches each tamper class with a precise typed error
//! (SPEC.md §19.5, §19.8, Annex D.5). These mirror the byte-level tamper
//! classes the tests/19 corpus asserts against.

mod common;

use std::io::{Cursor, Write};

use liasse_artifact::{Artifact, ArtifactError, LIASSE_JSON_PATH, STATE_PATH};
use liasse_ident::{Digest, DefinitionId};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

type Fallible = Result<(), Box<dyn std::error::Error>>;

#[test]
fn genuine_artifact_opens() -> Fallible {
    Artifact::open(&common::leaf_bytes()?)?;
    Ok(())
}

#[test]
fn modified_entry_fails_checksum() -> Fallible {
    // Flip the state section; its manifest checksum is now stale (Annex D.5).
    let tampered = common::repack_replacing(STATE_PATH, b"CORRUPTED-STATE-XXXX".to_vec())?;
    match Artifact::open(&tampered) {
        Err(ArtifactError::ChecksumMismatch { name, .. }) => assert_eq!(name, STATE_PATH),
        other => return Err(format!("expected checksum mismatch, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn missing_referenced_entry_fails() -> Fallible {
    // Strip the state entry, leaving its manifest record (Annex D.5).
    let tampered = common::repack_removing(STATE_PATH)?;
    match Artifact::open(&tampered) {
        Err(ArtifactError::MissingEntry { name }) => assert_eq!(name, STATE_PATH),
        other => return Err(format!("expected missing entry, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn swapped_liasse_json_with_stale_checksum_fails() -> Fallible {
    // Replace liasse.json without repairing checksums: the recorded entry
    // checksum no longer matches (Annex D.5).
    let tampered =
        common::repack_replacing(LIASSE_JSON_PATH, br#"{"$app":"evil@9.9.9"}"#.to_vec())?;
    match Artifact::open(&tampered) {
        Err(ArtifactError::ChecksumMismatch { name, .. }) => assert_eq!(name, LIASSE_JSON_PATH),
        other => return Err(format!("expected checksum mismatch, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn mimetype_mismatch_fails() -> Fallible {
    // §19.5: the mimetype entry must contain the exact media type.
    let tampered = common::repack_replacing("mimetype", b"application/zip".to_vec())?;
    match Artifact::open(&tampered) {
        Err(ArtifactError::MimetypeContent { .. }) => {}
        other => return Err(format!("expected mimetype content error, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn stale_definition_identity_rejected_at_open() -> Fallible {
    // SPEC.md Annex D.5 (item 21 pinned): verification MUST recompute the D.4
    // identity from the stored liasse.json and reject a stale
    // `manifest.definition.identity` even when every byte checksum matches —
    // internal self-consistency is mandatory, not opt-in.
    let new_definition = br#"{"$app":"t.hist@1.0.1","$liasse":1}"#.to_vec();
    let mut manifest = common::leaf_builder().manifest();
    let stale_identity = manifest.definition.identity;
    // Repair the entry checksum for the new bytes, but leave identity stale.
    manifest
        .entries
        .insert(LIASSE_JSON_PATH.to_owned(), liasse_artifact::EntryChecksum {
            media: "application/json".to_owned(),
            sha256: Digest::of_bytes(&new_definition),
        });

    let mut entries = common::entries_of(&common::leaf_bytes()?)?;
    for entry in &mut entries {
        match entry.0.as_str() {
            LIASSE_JSON_PATH => entry.1 = new_definition.clone(),
            "manifest.json" => entry.1 = manifest.to_canonical_bytes(),
            _ => {}
        }
    }
    let bytes = common::repack(entries)?;

    match Artifact::open(&bytes) {
        Err(ArtifactError::DefinitionIdentityMismatch { declared, computed }) => {
            assert_eq!(declared, stale_identity);
            assert_eq!(computed, DefinitionId::of_canonical_bytes(&new_definition));
        }
        other => return Err(format!("expected identity mismatch at open, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn duplicate_entry_is_rejected() -> Fallible {
    // Annex D.5: every referenced entry must exist exactly once. The `zip`
    // writer refuses duplicate names, so craft the raw bytes: two STORED empty
    // entries and two central-directory records, all named "dup". This is the
    // parser-differential a duplicate-entry attack exploits.
    let name = b"dup";
    let mut local = Vec::new();
    local.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]); // local file header
    local.extend_from_slice(&20u16.to_le_bytes()); // version needed
    local.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]); // flags, method, time, date
    local.extend_from_slice(&[0, 0, 0, 0]); // crc32 (empty data)
    local.extend_from_slice(&[0, 0, 0, 0]); // compressed size
    local.extend_from_slice(&[0, 0, 0, 0]); // uncompressed size
    local.extend_from_slice(&(name.len() as u16).to_le_bytes());
    local.extend_from_slice(&0u16.to_le_bytes()); // extra len
    local.extend_from_slice(name);

    let central = |offset: u32| {
        let mut c = Vec::new();
        c.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]); // central header
        c.extend_from_slice(&20u16.to_le_bytes()); // version made by
        c.extend_from_slice(&20u16.to_le_bytes()); // version needed
        c.extend_from_slice(&[0; 20]); // flags..uncompressed size (all zero)
        c.extend_from_slice(&(name.len() as u16).to_le_bytes());
        c.extend_from_slice(&[0; 8]); // extra, comment, disk, internal attrs
        c.extend_from_slice(&[0; 4]); // external attrs
        c.extend_from_slice(&offset.to_le_bytes()); // local header offset
        c.extend_from_slice(name);
        c
    };

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&local); // entry 1 @ 0
    let second_offset = bytes.len() as u32;
    bytes.extend_from_slice(&local); // entry 2
    let cd_offset = bytes.len() as u32;
    let cd1 = central(0);
    let cd2 = central(second_offset);
    let cd_size = (cd1.len() + cd2.len()) as u32;
    bytes.extend_from_slice(&cd1);
    bytes.extend_from_slice(&cd2);
    bytes.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]); // EOCD
    bytes.extend_from_slice(&[0, 0, 0, 0]); // disk numbers
    bytes.extend_from_slice(&2u16.to_le_bytes()); // entries this disk
    bytes.extend_from_slice(&2u16.to_le_bytes()); // total entries
    bytes.extend_from_slice(&cd_size.to_le_bytes());
    bytes.extend_from_slice(&cd_offset.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes()); // comment len

    match liasse_artifact::Archive::read(&bytes) {
        Err(ArtifactError::DuplicateEntry { name }) => assert_eq!(name, "dup"),
        other => return Err(format!("expected duplicate-entry error, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn path_traversal_entry_is_rejected() -> Fallible {
    // Zip-slip: an entry escaping the archive root is not part of the §19.5
    // structure and is rejected at container read.
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    writer.start_file("../evil.txt", options)?;
    writer.write_all(b"pwned")?;
    let bytes = writer.finish()?.into_inner();

    match liasse_artifact::Archive::read(&bytes) {
        Err(ArtifactError::EntryOutsideRoot { .. }) => {}
        other => return Err(format!("expected path-traversal error, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn missing_manifest_is_rejected() -> Fallible {
    let tampered = common::repack_removing("manifest.json")?;
    match Artifact::open(&tampered) {
        Err(ArtifactError::ManifestMissing) => {}
        other => return Err(format!("expected manifest-missing error, got {other:?}").into()),
    }
    Ok(())
}

/// Repack the sample leaf artifact with a mutated manifest (§19.5 admission tests).
fn repack_with_manifest(manifest: &liasse_artifact::Manifest) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let canonical = manifest.to_canonical_bytes();
    let mut entries = common::entries_of(&common::leaf_bytes()?)?;
    for entry in &mut entries {
        if entry.0 == "manifest.json" {
            entry.1 = canonical.clone();
        }
    }
    Ok(common::repack(entries)?)
}

#[test]
fn entries_missing_required_leaf_is_rejected() -> Fallible {
    // §19.5 (SPEC-ISSUES #33): `entries` MUST cover the required leaf
    // `state/current.cbor.zst`. Dropping its row — while keeping the file and the
    // `state` role member — leaves its media type recorded nowhere (D.5) and is
    // rejected at admission, not silently under-verified.
    let mut manifest = common::leaf_builder().manifest();
    assert!(manifest.entries.remove(STATE_PATH).is_some(), "sample builder records the state leaf");
    let bytes = repack_with_manifest(&manifest)?;
    match Artifact::open(&bytes) {
        Err(ArtifactError::EntriesMissingRequired { path }) => assert_eq!(path, STATE_PATH),
        other => return Err(format!("expected entries-missing-required, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn entries_listing_module_artifact_is_rejected() -> Fallible {
    // §19.5 (SPEC-ISSUES #33): a nested child artifact under `modules/` is
    // inventoried by `included_modules`, never listed in `entries`. Its checksum
    // already covers the child's exact bytes; a duplicate `entries` row is the
    // maximal (double-inventory) reading the pin rejects.
    let mut manifest = common::leaf_builder().manifest();
    manifest.entries.insert(
        "modules/child-1.liasse".to_owned(),
        liasse_artifact::EntryChecksum {
            media: "application/vnd.liasse+zip".to_owned(),
            sha256: Digest::of_bytes(b"child-bytes"),
        },
    );
    let bytes = repack_with_manifest(&manifest)?;
    match Artifact::open(&bytes) {
        Err(ArtifactError::EntriesForbiddenMember { path }) => {
            assert_eq!(path, "modules/child-1.liasse");
        }
        other => return Err(format!("expected entries-forbidden-member, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn genuine_artifact_still_opens_under_entries_membership() -> Fallible {
    // The canonical set (four structural leaves, no manifest.json, no modules/*)
    // remains accepted (§19.5 #33 accept side).
    Artifact::open(&common::leaf_bytes()?)?;
    Ok(())
}
