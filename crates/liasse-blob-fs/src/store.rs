//! The content-addressed object store on the local filesystem (§18.9).
//!
//! An object lives at a path derived from its 64-byte SHA-512, sharded two
//! levels deep (`root/<hex[0..2]>/<hex[2..4]>/<hex>`) so no single directory
//! holds the whole population and so identical content maps to one path —
//! deduplicated and idempotent to copy (§18.9). The application filename
//! (`$name`) never influences placement.
//!
//! Writes are staged then committed with an atomic rename (§18.7): bytes land in
//! a uniquely named temporary object under `root/.staging`, are flushed and
//! synced, then renamed into the content path. An interrupted or rejected upload
//! leaves only the temporary object, which `tempfile` drops — never a
//! half-committed object. Concurrent uploads of the same content each rename
//! onto the same final path; the rename is atomic and the bytes are identical.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use liasse_host::{ByteRange, ConnectorFailure, UsageObservation};
use liasse_value::Sha512;
use tempfile::NamedTempFile;

/// The number of leading hex characters in each of the two shard levels.
const SHARD_WIDTH: usize = 2;
/// The canonical content-hash name length: 64 bytes as lowercase hex.
const CONTENT_NAME_LEN: usize = 128;
/// The staging subdirectory holding in-flight temporary objects. Its name is not
/// a valid shard prefix, so it never collides with the content tree.
const STAGING_DIR: &str = ".staging";

/// A content-addressed object store rooted at a directory.
pub(crate) struct ContentStore {
    root: PathBuf,
    staging: PathBuf,
}

impl ContentStore {
    /// A store rooted at `root`. Directories are created lazily on first write,
    /// so construction cannot fail and a fresh root needs no provisioning.
    pub(crate) fn new(root: PathBuf) -> Self {
        let staging = root.join(STAGING_DIR);
        Self { root, staging }
    }

    /// The content-addressed path an object for `digest` occupies.
    fn object_path(&self, digest: &Sha512) -> Result<PathBuf, ConnectorFailure> {
        let hex = digest.to_canonical_text();
        let first = hex.get(0..SHARD_WIDTH);
        let second = hex.get(SHARD_WIDTH..SHARD_WIDTH * 2);
        match (first, second) {
            (Some(first), Some(second)) => Ok(self.root.join(first).join(second).join(&hex)),
            // A `Sha512` is always 128 hex chars, so this is unreachable in
            // practice; fail loud rather than index and risk a panic.
            _ => Err(ConnectorFailure::Failed(format!(
                "content hash `{hex}` is not a {CONTENT_NAME_LEN}-character name"
            ))),
        }
    }

    /// Whether a committed object for `digest` is present.
    pub(crate) fn exists(&self, digest: &Sha512) -> Result<bool, ConnectorFailure> {
        Ok(self.object_path(digest)?.is_file())
    }

    /// Commit `bytes` at the content path for `digest`. Idempotent: an object
    /// already present is the same content (content-addressed), so the write is
    /// skipped and the copy deduplicates (§18.9). The caller verifies that
    /// `bytes` hash to `digest` before staging (§18.9 ingress).
    pub(crate) fn commit(&self, digest: &Sha512, bytes: &[u8]) -> Result<(), ConnectorFailure> {
        let path = self.object_path(digest)?;
        if path.is_file() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| Self::failed("create object directory", &error))?;
        }
        fs::create_dir_all(&self.staging)
            .map_err(|error| Self::failed("create staging directory", &error))?;

        let mut staged = NamedTempFile::new_in(&self.staging)
            .map_err(|error| Self::failed("open staging object", &error))?;
        staged
            .write_all(bytes)
            .map_err(|error| Self::failed("write staging object", &error))?;
        staged
            .as_file()
            .sync_all()
            .map_err(|error| Self::failed("sync staging object", &error))?;
        staged
            .persist(&path)
            .map_err(|error| Self::failed("commit object", &error.error))?;
        Ok(())
    }

    /// Read the whole object for `digest`. The bytes are unverified here; the
    /// connector hash-checks them before delivery (§18.9).
    pub(crate) fn read(&self, digest: &Sha512) -> Result<Vec<u8>, ConnectorFailure> {
        let path = self.object_path(digest)?;
        fs::read(&path).map_err(|error| Self::read_failure(&error))
    }

    /// Read the half-open byte `range` of the object for `digest` (§18.8/§18.12).
    /// A single range cannot be hash-verified against the whole-object digest;
    /// the caller assembling the whole object verifies it (§18.8).
    pub(crate) fn read_range(
        &self,
        digest: &Sha512,
        range: ByteRange,
    ) -> Result<Vec<u8>, ConnectorFailure> {
        let path = self.object_path(digest)?;
        let mut file = File::open(&path).map_err(|error| Self::read_failure(&error))?;
        let len = file
            .metadata()
            .map_err(|error| Self::failed("stat object", &error))?
            .len();
        if range.end() > len {
            return Err(ConnectorFailure::RangeOutOfBounds {
                start: range.start(),
                end: range.end(),
                len,
            });
        }
        file.seek(SeekFrom::Start(range.start()))
            .map_err(|error| Self::failed("seek object", &error))?;
        let span = usize::try_from(range.len())
            .map_err(|_| ConnectorFailure::Failed("range length exceeds addressable memory".into()))?;
        let mut buffer = vec![0u8; span];
        file.read_exact(&mut buffer)
            .map_err(|error| Self::failed("read object range", &error))?;
        Ok(buffer)
    }

    /// Delete the object for `digest`. Idempotent: a missing object is success.
    pub(crate) fn remove(&self, digest: &Sha512) -> Result<(), ConnectorFailure> {
        let path = self.object_path(digest)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Self::failed("delete object", &error)),
        }
    }

    /// The connector-reported physical usage: the count and total byte size of
    /// committed content objects, excluding in-flight staging (§18.11).
    pub(crate) fn usage(&self) -> Result<UsageObservation, ConnectorFailure> {
        let mut object_count = 0u64;
        let mut physical_bytes = 0u64;
        self.accumulate(&self.root, &mut object_count, &mut physical_bytes)?;
        Ok(UsageObservation {
            object_count,
            physical_bytes,
        })
    }

    /// Recursively tally committed content objects under `dir`.
    fn accumulate(
        &self,
        dir: &Path,
        object_count: &mut u64,
        physical_bytes: &mut u64,
    ) -> Result<(), ConnectorFailure> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            // A store that has never been written has no root directory yet.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(Self::failed("scan store", &error)),
        };
        for entry in entries {
            let entry = entry.map_err(|error| Self::failed("scan store entry", &error))?;
            let file_type = entry
                .file_type()
                .map_err(|error| Self::failed("inspect store entry", &error))?;
            let path = entry.path();
            if file_type.is_dir() {
                if path == self.staging {
                    continue;
                }
                self.accumulate(&path, object_count, physical_bytes)?;
            } else if file_type.is_file() && Self::is_content_name(&entry.file_name()) {
                let metadata = entry
                    .metadata()
                    .map_err(|error| Self::failed("stat store object", &error))?;
                *object_count += 1;
                *physical_bytes += metadata.len();
            }
        }
        Ok(())
    }

    /// Whether `name` is a canonical content-hash object name (128 lowercase hex
    /// characters), so staging temporaries and stray files are not counted.
    fn is_content_name(name: &OsStr) -> bool {
        name.to_str().is_some_and(|name| {
            name.len() == CONTENT_NAME_LEN
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    }

    /// Map an I/O failure of `action` to a typed connector failure (§18.12).
    fn failed(action: &str, error: &io::Error) -> ConnectorFailure {
        ConnectorFailure::Failed(format!("{action}: {error}"))
    }

    /// Map a read I/O failure, translating a missing object to `NotFound`.
    fn read_failure(error: &io::Error) -> ConnectorFailure {
        if error.kind() == io::ErrorKind::NotFound {
            ConnectorFailure::NotFound
        } else {
            Self::failed("read object", error)
        }
    }
}
