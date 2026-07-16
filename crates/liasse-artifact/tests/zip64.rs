//! ZIP64 handling (SPEC.md §4.1: "a ZIP64 archive").
//!
//! A real >4 GiB payload is unreasonable to materialize in a test. Instead the
//! builder always sets the ZIP64 large-file flag, so every artifact genuinely
//! requires ZIP64 support: this is verified by the presence of the ZIP64
//! End-Of-Central-Directory locator signature. A second test drives the
//! separate ZIP64 trigger — a central directory of more than 65535 entries,
//! which overflows the classic 16-bit count field — and round-trips it.

mod common;

use liasse_artifact::{Archive, ArchiveBuilder};

type Fallible = Result<(), Box<dyn std::error::Error>>;

/// The ZIP64 End-Of-Central-Directory locator signature (PK\x06\x07).
const ZIP64_EOCD_LOCATOR: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn built_artifact_declares_zip64() -> Fallible {
    // The large-file flag forces the ZIP64 EOCD record + locator, so the
    // container advertises the ZIP64 format §4.1 mandates.
    let bytes = common::leaf_bytes()?;
    assert!(
        contains(&bytes, &ZIP64_EOCD_LOCATOR),
        "built artifact should carry the ZIP64 EOCD locator"
    );
    // And it still reads back correctly through the ZIP64 path.
    Archive::read(&bytes)?;
    Ok(())
}

#[test]
fn malicious_zip64_eocd_offset_is_a_typed_error_not_a_panic() {
    // A classic EOCD whose 16-bit entry count is saturated points at the ZIP64
    // locator; a locator whose declared ZIP64-EOCD offset is `u64::MAX` must be
    // rejected by bounds-checking, never by an arithmetic overflow panic.
    let mut b = Vec::new();
    // ZIP64 EOCD locator (20 bytes) at offset 0.
    b.extend_from_slice(&ZIP64_EOCD_LOCATOR);
    b.extend_from_slice(&0u32.to_le_bytes()); // disk holding the ZIP64 EOCD
    b.extend_from_slice(&u64::MAX.to_le_bytes()); // out-of-range ZIP64 EOCD offset
    b.extend_from_slice(&1u32.to_le_bytes()); // total disks
    // Classic EOCD (22 bytes) at offset 20.
    b.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    b.extend_from_slice(&0u16.to_le_bytes()); // this disk
    b.extend_from_slice(&0u16.to_le_bytes()); // disk with the central directory
    b.extend_from_slice(&0u16.to_le_bytes()); // entries on this disk
    b.extend_from_slice(&0xFFFFu16.to_le_bytes()); // total entries -> ZIP64 trigger
    b.extend_from_slice(&0u32.to_le_bytes()); // central-directory size
    b.extend_from_slice(&0u32.to_le_bytes()); // central-directory offset
    b.extend_from_slice(&0u16.to_le_bytes()); // comment length

    assert!(Archive::read(&b).is_err());
}

#[test]
fn many_entries_overflow_16bit_count_and_round_trip() -> Fallible {
    // >65535 entries forces the ZIP64 EOCD (the classic count field is 16-bit).
    let count = 70_000u32;
    let mut builder = ArchiveBuilder::new();
    for i in 0..count {
        builder.add(format!("e/{i:05}"), Vec::new());
    }
    let bytes = builder.finish()?;
    assert!(contains(&bytes, &ZIP64_EOCD_LOCATOR));

    let archive = Archive::read(&bytes)?;
    assert_eq!(archive.entries().len(), count as usize);
    Ok(())
}
