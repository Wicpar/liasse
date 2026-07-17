//! A minimal STORED-only ZIP read/write layer for artifact byte surgery (§4.1,
//! §19.5, Annex D.5).
//!
//! The artifact tamper corpus (tests/04, tests/annex-d) needs to *produce* an
//! archive with a duplicate entry name — the parser-differential Annex D.5
//! rejects — which the `zip` crate's writer refuses to emit. This layer reads an
//! archive into an ordered `(name, bytes)` list ([`read_ordered`], via the
//! container reader that rejects a duplicate) and re-emits an ordered list
//! verbatim ([`write_ordered`]), *including* duplicates and preserving entry
//! order, so a surgery step can inject exactly the collision under test. Output
//! is a classic STORED ZIP with a correct per-entry CRC-32, so an honest read
//! ([`liasse_artifact::Archive`]) accepts a non-tampered result and re-verifies a
//! tampered one against the manifest.

use liasse_artifact::Archive;

/// A little-endian ZIP field width fits every real artifact entry (all far under
/// 4 GiB); a pathological oversize would corrupt the field, never panic.
fn u32_of(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// Read an archive into its entries in stored order, rejecting a duplicate name
/// or path traversal exactly as [`Archive::read`] does (the input to a surgery
/// step is always a well-formed archive; the duplicate is introduced *by* the
/// step).
pub(super) fn read_ordered(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let archive = Archive::read(bytes).map_err(|err| err.to_string())?;
    Ok(archive.entries().iter().map(|entry| (entry.name().to_owned(), entry.data().to_vec())).collect())
}

/// Emit an ordered `(name, bytes)` list as a classic STORED ZIP, preserving order
/// and any duplicate names. Every entry carries a correct CRC-32 and STORED
/// sizes, so an honest reader reads it back; a duplicate name is caught by that
/// reader's exactly-once rule.
pub(super) fn write_ordered(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = u32_of(body.len());
        let crc = crc32fast::hash(data);
        let size = u32_of(data.len());
        let name_bytes = name.as_bytes();
        let name_len = u16::try_from(name_bytes.len()).unwrap_or(u16::MAX);

        // Local file header (30 bytes + name + data).
        body.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
        body.extend_from_slice(&20u16.to_le_bytes()); // version needed
        body.extend_from_slice(&[0, 0]); // general-purpose flags
        body.extend_from_slice(&[0, 0]); // method: stored
        body.extend_from_slice(&[0, 0, 0, 0]); // mod time + date (fixed)
        body.extend_from_slice(&crc.to_le_bytes());
        body.extend_from_slice(&size.to_le_bytes()); // compressed size
        body.extend_from_slice(&size.to_le_bytes()); // uncompressed size
        body.extend_from_slice(&name_len.to_le_bytes());
        body.extend_from_slice(&[0, 0]); // extra length
        body.extend_from_slice(name_bytes);
        body.extend_from_slice(data);

        // Central-directory record (46 bytes + name).
        central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&[0, 0]); // flags
        central.extend_from_slice(&[0, 0]); // method: stored
        central.extend_from_slice(&[0, 0, 0, 0]); // mod time + date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes()); // compressed size
        central.extend_from_slice(&size.to_le_bytes()); // uncompressed size
        central.extend_from_slice(&name_len.to_le_bytes());
        central.extend_from_slice(&[0, 0]); // extra length
        central.extend_from_slice(&[0, 0]); // comment length
        central.extend_from_slice(&[0, 0]); // disk number start
        central.extend_from_slice(&[0, 0]); // internal attributes
        central.extend_from_slice(&[0, 0, 0, 0]); // external attributes
        central.extend_from_slice(&offset.to_le_bytes()); // local-header offset
        central.extend_from_slice(name_bytes);
    }

    let cd_offset = u32_of(body.len());
    let cd_size = u32_of(central.len());
    let count = u16::try_from(entries.len()).unwrap_or(u16::MAX);

    let mut out = body;
    out.extend_from_slice(&central);
    out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]); // end of central directory
    out.extend_from_slice(&[0, 0]); // this disk number
    out.extend_from_slice(&[0, 0]); // central-directory start disk
    out.extend_from_slice(&count.to_le_bytes()); // records this disk
    out.extend_from_slice(&count.to_le_bytes()); // records total
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&[0, 0]); // archive comment length
    out
}
