//! The ZIP64 container layer (SPEC.md §4.1, §19.5, Annex D.5).
//!
//! Reading is parse-don't-validate at the *container* level: a constructed
//! [`Archive`] is proof the bytes are a readable ZIP whose file entries all stay
//! inside the archive root and each occur exactly once. Path traversal
//! (zip-slip) and duplicate names — both parser-differential attack vectors the
//! corpus exercises — are rejected here, before any manifest is consulted.
//!
//! Writing is deterministic: the same entries always produce byte-identical
//! output, which is what the definition identity (§4.2, D.4) and round-trip
//! guarantees (§19.10) rely on. The builder emits `mimetype` first, then every
//! other entry in Unicode-scalar name order, all STORED (uncompressed) with a
//! fixed timestamp and permissions. Every entry sets the ZIP64 large-file flag
//! so the container genuinely requires ZIP64 support (§4.1), regardless of size.

use std::collections::HashSet;
use std::io::{Cursor, Read, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

use crate::error::ArtifactError;
use crate::raw;

/// One file entry read from an archive.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    name: String,
    data: Vec<u8>,
    stored: bool,
}

impl ArchiveEntry {
    /// The entry's archive path (always `/`-separated, inside the root).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The exact entry bytes.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Whether the entry was stored uncompressed.
    #[must_use]
    pub fn is_stored(&self) -> bool {
        self.stored
    }
}

/// A readable ZIP archive with unique, root-enclosed file entries in their
/// stored order.
#[derive(Debug, Clone)]
pub struct Archive {
    entries: Vec<ArchiveEntry>,
}

impl Archive {
    /// Read a ZIP byte stream, rejecting path traversal and duplicate names.
    ///
    /// Uniqueness is checked against the *raw* central-directory record list
    /// (see [`raw`]), not the `zip` crate's by-name table, which silently
    /// collapses two records that share a name — the duplicate-entry attack
    /// Annex D.5 rejects.
    pub fn read(bytes: &[u8]) -> Result<Self, ArtifactError> {
        let mut seen: HashSet<&str> = HashSet::new();
        let names = raw::central_directory_names(bytes)?;
        for name in &names {
            if name.ends_with('/') {
                continue; // a directory marker, not a content entry
            }
            if escapes_root(name) {
                return Err(ArtifactError::EntryOutsideRoot { name: name.clone() });
            }
            if !seen.insert(name.as_str()) {
                return Err(ArtifactError::DuplicateEntry { name: name.clone() });
            }
        }

        let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(|e| ArtifactError::NotZip {
            detail: e.to_string(),
        })?;
        let mut entries: Vec<ArchiveEntry> = Vec::with_capacity(zip.len());
        for index in 0..zip.len() {
            let mut file = zip.by_index(index).map_err(|e| ArtifactError::NotZip {
                detail: e.to_string(),
            })?;
            if file.is_dir() {
                continue;
            }
            let name = file.name().to_owned();
            if file.enclosed_name().is_none() {
                return Err(ArtifactError::EntryOutsideRoot { name });
            }
            let stored = file.compression() == CompressionMethod::Stored;
            let mut data = Vec::with_capacity(usize::try_from(file.size()).unwrap_or(0));
            file.read_to_end(&mut data).map_err(|e| ArtifactError::NotZip {
                detail: format!("reading entry `{name}`: {e}"),
            })?;
            entries.push(ArchiveEntry { name, data, stored });
        }
        Ok(Self { entries })
    }

    /// The entries in stored order.
    #[must_use]
    pub fn entries(&self) -> &[ArchiveEntry] {
        &self.entries
    }

    /// The first stored entry, if any (the `mimetype` position, §19.5).
    #[must_use]
    pub fn first(&self) -> Option<&ArchiveEntry> {
        self.entries.first()
    }

    /// The entry with the given name. Names are unique, so at most one matches.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ArchiveEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

/// Whether an archive entry name escapes the artifact root: an absolute path, a
/// backslash (Windows) path, a drive prefix, or any `..` component (zip-slip).
fn escapes_root(name: &str) -> bool {
    if name.starts_with('/') || name.contains('\\') {
        return true;
    }
    let first_component = name.split('/').next().unwrap_or(name);
    if first_component.contains(':') {
        return true; // a drive or scheme prefix
    }
    name.split('/').any(|component| component == "..")
}

/// A deterministic ZIP64 archive builder.
#[derive(Debug, Default)]
pub struct ArchiveBuilder {
    entries: std::collections::HashMap<String, Vec<u8>>,
}

impl ArchiveBuilder {
    /// Start an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one entry. A later add with the same name replaces the earlier one,
    /// keeping the builder single-valued per name. Entry order is imposed at
    /// [`finish`](Self::finish), so insertion order never affects output.
    pub fn add(&mut self, name: impl Into<String>, data: Vec<u8>) {
        self.entries.insert(name.into(), data);
    }

    /// Serialize to deterministic ZIP64 bytes: `mimetype` first, then every
    /// other entry in name order, all STORED with a fixed timestamp. The
    /// ZIP64 end-of-central-directory record is always emitted (via an empty
    /// ZIP64 comment) so every artifact is a genuine ZIP64 archive (§4.1),
    /// independent of size.
    pub fn finish(self) -> Result<Vec<u8>, ArtifactError> {
        let mut ordered: Vec<(String, Vec<u8>)> = self.entries.into_iter().collect();
        ordered.sort_by(|(a, _), (b, _)| match (a.as_str(), b.as_str()) {
            ("mimetype", "mimetype") => std::cmp::Ordering::Equal,
            ("mimetype", _) => std::cmp::Ordering::Less,
            (_, "mimetype") => std::cmp::Ordering::Greater,
            (left, right) => left.cmp(right),
        });

        // A fixed ZIP epoch (1980-01-01 00:00:00) so output is byte-reproducible;
        // the components are always in range, so the fallback is never taken.
        let epoch = DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0)
            .unwrap_or_else(|_| DateTime::default_for_write());
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .last_modified_time(epoch)
            .unix_permissions(0o644)
            .large_file(true);

        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer.set_zip64_comment(Some(String::new()));
        for (name, data) in ordered {
            writer
                .start_file(&name, options)
                .map_err(|e| ArtifactError::ContainerWrite {
                    detail: format!("starting entry `{name}`: {e}"),
                })?;
            writer.write_all(&data).map_err(|e| ArtifactError::ContainerWrite {
                detail: format!("writing entry `{name}`: {e}"),
            })?;
        }
        let cursor = writer.finish().map_err(|e| ArtifactError::ContainerWrite {
            detail: e.to_string(),
        })?;
        Ok(cursor.into_inner())
    }
}
