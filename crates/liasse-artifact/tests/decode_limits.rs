#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Bounded decode of an untrusted `.liasse` (SPEC.md §4.1 container): a
//! decompression-bomb (zip-bomb) or a forged declared size must be rejected
//! LOUDLY with BOUNDED memory — never inflated to OOM before the check.
//!
//! A `.liasse` may be user-supplied, so [`Archive::read`] never trusts the
//! central-directory uncompressed size: it bounds the *actual* inflated bytes
//! per entry, caps the total across entries, and caps the entry count. Each
//! probe here is Ok before the caps existed (the full payload materialized) and
//! a loud, bounded `Err` after.

mod common;

use std::io::Write;

use liasse_artifact::{decode_package_from_blob, Archive, ArchiveBuilder, ArtifactError};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// The per-entry decode cap the fix enforces (kept in sync with `archive.rs`).
const PER_ENTRY_CAP: u64 = 32 * 1024 * 1024;
/// The total-uncompressed decode cap the fix enforces.
const TOTAL_CAP: u64 = 64 * 1024 * 1024;
/// The entry-count decode cap the fix enforces (above the ZIP64 count-overflow
/// point, so a legitimate >65535-entry container still reads).
const ENTRY_COUNT_CAP: usize = 131_072;

/// A ZIP whose named entries are DEFLATE-compressed runs of zero bytes of the
/// given *uncompressed* length: a few hundred KiB of archive that declares (and,
/// read naively, inflates to) hundreds of MiB. The write streams the payload in
/// small chunks, so building the fixture never itself holds the full length.
fn deflate_archive(entries: &[(&str, usize)]) -> Vec<u8> {
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true);
    let chunk = vec![0u8; 64 * 1024];
    for (name, uncompressed_len) in entries {
        writer.start_file(*name, options).expect("start entry");
        let mut remaining = *uncompressed_len;
        while remaining > 0 {
            let n = remaining.min(chunk.len());
            writer.write_all(chunk.get(..n).expect("in-bounds chunk slice")).expect("write payload");
            remaining -= n;
        }
    }
    writer.finish().expect("finish archive").into_inner()
}

#[test]
fn a_deflate_zip_bomb_is_rejected_bounded_not_oom() {
    // The empirically confirmed repro: a single DEFLATE member inflating to
    // 256 MiB (~1029x amplification) from a tiny archive.
    let declared = 256 * 1024 * 1024;
    let bomb = deflate_archive(&[("mimetype", declared)]);

    // Tiny in, huge declared out — the amplification a naive decoder materialized.
    assert!(
        bomb.len() < 1024 * 1024,
        "the bomb archive is tiny ({} bytes) yet declares {declared} uncompressed \
         (~{}x amplification)",
        bomb.len(),
        declared / bomb.len().max(1),
    );

    // Before the fix `Archive::read` inflated the full 256 MiB before any check;
    // now it stops at the per-entry cap and rejects LOUDLY, having read at most
    // that cap — hundreds of MiB are never materialized.
    let error = Archive::read(&bomb).expect_err("the zip bomb is rejected");
    match error {
        ArtifactError::EntryTooLarge { limit, .. } => {
            assert_eq!(limit, PER_ENTRY_CAP, "rejected at the per-entry cap");
            assert!(
                PER_ENTRY_CAP < declared as u64,
                "the cap ({PER_ENTRY_CAP}) is far below the declared/inflated size",
            );
        }
        other => panic!("expected a bounded EntryTooLarge, got {other:?}"),
    }

    // The runtime's public decode entry point surfaces the same loud rejection.
    let error = decode_package_from_blob(&bomb).expect_err("decode also rejects the bomb");
    assert!(
        matches!(error, ArtifactError::EntryTooLarge { .. }),
        "decode_package_from_blob rejects the bomb boundedly, got {error:?}",
    );
}

#[test]
fn the_total_uncompressed_size_is_capped_across_entries() {
    // Three members, each UNDER the per-entry cap, that together exceed the total
    // cap: no single entry trips `EntryTooLarge`, but the running total does.
    let each = 30 * 1024 * 1024; // < PER_ENTRY_CAP, and 3 * 30 MiB > TOTAL_CAP
    let bomb = deflate_archive(&[("a", each), ("b", each), ("c", each)]);

    let error = Archive::read(&bomb).expect_err("the aggregate bomb is rejected");
    match error {
        ArtifactError::ArchiveTooLarge { limit } => {
            assert_eq!(limit, TOTAL_CAP, "rejected at the total cap");
        }
        other => panic!("expected a bounded ArchiveTooLarge, got {other:?}"),
    }
}

#[test]
fn the_entry_count_is_capped() {
    // One past the cap: rejected from the raw central-directory count, before the
    // ZIP is parsed or any entry is inflated.
    let mut builder = ArchiveBuilder::new();
    for index in 0..=ENTRY_COUNT_CAP {
        builder.add(format!("e{index:05}"), b"x".to_vec());
    }
    let padded = builder.finish().expect("archive builds");

    let error = Archive::read(&padded).expect_err("an entry-padded archive is rejected");
    match error {
        ArtifactError::TooManyEntries { limit } => {
            assert_eq!(limit, ENTRY_COUNT_CAP, "rejected at the entry-count cap");
        }
        other => panic!("expected a bounded TooManyEntries, got {other:?}"),
    }
}

#[test]
fn a_legitimate_package_still_decodes_under_the_caps() {
    // The caps are generous: a real (small) package artifact opens and decodes
    // unaffected.
    let bytes = common::leaf_bytes().expect("the sample artifact builds");
    let archive = Archive::read(&bytes).expect("a legitimate package still reads");
    assert!(
        archive.get("mimetype").is_some(),
        "the legitimate package's entries are present after a bounded read",
    );
    decode_package_from_blob(&bytes).expect("a legitimate package still decodes");
}
