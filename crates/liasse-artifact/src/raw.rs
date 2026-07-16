//! A minimal raw central-directory scan (SPEC.md §19.5, Annex D.5).
//!
//! The `zip` crate keys its entry table by name, silently collapsing two
//! central-directory records that share a name — exactly the parser-differential
//! a duplicate-entry attack relies on (Annex D.5: every referenced entry must
//! exist *exactly once*). To reject it, the true central-directory record list
//! must be read independently of that table. This scan reads only what it needs
//! — the End-Of-Central-Directory record (and its ZIP64 form) and each central
//! header's file name — to produce the authoritative name list, over which
//! [`Archive`](crate::Archive) enforces uniqueness and root-enclosure.
//!
//! All reads are bounds-checked (`indexing_slicing` is denied workspace-wide);
//! any malformed offset yields [`ArtifactError::NotZip`], never a panic.

use crate::error::ArtifactError;

const EOCD_SIG: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
const ZIP64_LOCATOR_SIG: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
const ZIP64_EOCD_SIG: [u8; 4] = [0x50, 0x4b, 0x06, 0x06];
const CENTRAL_SIG: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];

/// A bounds-checked little-endian view over the archive bytes.
struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn slice(&self, at: usize, len: usize) -> Result<&[u8], ArtifactError> {
        let end = at.checked_add(len).ok_or_else(offset_error)?;
        self.0.get(at..end).ok_or_else(offset_error)
    }

    fn u16(&self, at: usize) -> Result<u16, ArtifactError> {
        let raw: [u8; 2] = self.slice(at, 2)?.try_into().map_err(|_| offset_error())?;
        Ok(u16::from_le_bytes(raw))
    }

    fn u32(&self, at: usize) -> Result<u32, ArtifactError> {
        let raw: [u8; 4] = self.slice(at, 4)?.try_into().map_err(|_| offset_error())?;
        Ok(u32::from_le_bytes(raw))
    }

    fn u64(&self, at: usize) -> Result<u64, ArtifactError> {
        let raw: [u8; 8] = self.slice(at, 8)?.try_into().map_err(|_| offset_error())?;
        Ok(u64::from_le_bytes(raw))
    }
}

fn offset_error() -> ArtifactError {
    ArtifactError::NotZip {
        detail: "central directory offset is out of range".to_owned(),
    }
}

fn as_usize(value: u64) -> Result<usize, ArtifactError> {
    usize::try_from(value).map_err(|_| offset_error())
}

/// Where the central directory begins and how many records it holds.
struct Directory {
    offset: usize,
    count: u64,
}

/// The file names of every central-directory record, in directory order,
/// including any duplicates (the whole point of reading them here).
pub fn central_directory_names(bytes: &[u8]) -> Result<Vec<String>, ArtifactError> {
    let view = Bytes(bytes);
    let eocd = find_eocd(bytes)?;
    let directory = locate_directory(&view, eocd)?;

    let mut names = Vec::new();
    let mut cursor = directory.offset;
    for _ in 0..directory.count {
        if view.slice(cursor, 4)? != CENTRAL_SIG {
            return Err(ArtifactError::NotZip {
                detail: "malformed central-directory record".to_owned(),
            });
        }
        let name_len = view.u16(cursor + 28)? as usize;
        let extra_len = view.u16(cursor + 30)? as usize;
        let comment_len = view.u16(cursor + 32)? as usize;
        let name_bytes = view.slice(cursor + 46, name_len)?;
        names.push(String::from_utf8_lossy(name_bytes).into_owned());
        cursor = cursor
            .checked_add(46 + name_len + extra_len + comment_len)
            .ok_or_else(offset_error)?;
    }
    Ok(names)
}

/// Find the byte offset of the End-Of-Central-Directory record (the last one).
fn find_eocd(bytes: &[u8]) -> Result<usize, ArtifactError> {
    let min = bytes.len().checked_sub(22).ok_or_else(|| ArtifactError::NotZip {
        detail: "archive too small for an EOCD record".to_owned(),
    })?;
    for start in (0..=min).rev() {
        if bytes.get(start..start + 4) == Some(&EOCD_SIG[..]) {
            return Ok(start);
        }
    }
    Err(ArtifactError::NotZip {
        detail: "no End-Of-Central-Directory record".to_owned(),
    })
}

/// Resolve the directory offset and record count, following the ZIP64 records
/// whenever the classic EOCD fields carry their overflow markers.
fn locate_directory(view: &Bytes<'_>, eocd: usize) -> Result<Directory, ArtifactError> {
    let count16 = view.u16(eocd + 10)?;
    let offset32 = view.u32(eocd + 16)?;
    if count16 != 0xFFFF && offset32 != 0xFFFF_FFFF {
        return Ok(Directory {
            offset: as_usize(u64::from(offset32))?,
            count: u64::from(count16),
        });
    }

    // ZIP64: the locator sits 20 bytes before the classic EOCD.
    let locator = eocd.checked_sub(20).ok_or_else(offset_error)?;
    if view.slice(locator, 4)? != ZIP64_LOCATOR_SIG {
        return Err(ArtifactError::NotZip {
            detail: "ZIP64 markers present but no ZIP64 locator".to_owned(),
        });
    }
    let zip64_eocd = as_usize(view.u64(locator + 8)?)?;
    if view.slice(zip64_eocd, 4)? != ZIP64_EOCD_SIG {
        return Err(ArtifactError::NotZip {
            detail: "ZIP64 EOCD record not found".to_owned(),
        });
    }
    Ok(Directory {
        offset: as_usize(view.u64(zip64_eocd + 48)?)?,
        count: view.u64(zip64_eocd + 32)?,
    })
}
