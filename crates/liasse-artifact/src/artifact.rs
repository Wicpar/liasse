//! Opening and verifying a `.liasse` artifact (SPEC.md §4.1, §19.5, §19.8,
//! Annex D.5).
//!
//! [`Artifact::open`] is the parse-don't-validate boundary: a returned
//! [`Artifact`] is proof the byte stream is a structurally well-formed,
//! integrity-verified `.liasse` archive — the `mimetype` is correct and first,
//! `manifest.json` matches the closed format-1 structure, every entry the
//! manifest references exists exactly once with a matching checksum, and every
//! nested module artifact is itself a valid, verified `.liasse` (the recursive
//! verification §19.8 requires). Each failure is a typed [`ArtifactError`]
//! naming the offending entry.
//!
//! State and history section *contents* are opaque bytes here: this layer
//! verifies their checksums and hands them back verbatim. Decoding the CBOR
//! state, parsing `history/index.json`, and the §19 merge/reconcile semantics
//! belong to the runtime above.
//!
//! Internal self-consistency is mandatory (SPEC.md Annex D.5, item 21 pinned):
//! `open` recomputes the D.4 definition identity from the stored `liasse.json`
//! and rejects a stale or lying `manifest.definition.identity`. Unknown extra
//! archive entries not referenced by the manifest are **tolerated** for forward
//! compatibility (Annex D.5); they carry no verified status. The Annex D.5
//! state-section embedded `definition`-digest cross-check is an honest hole:
//! the implemented state section does not yet carry that member (its shape is a
//! documented seam), so the check lands with it.
//!
//! ## Deliberately not enforced by `open`
//!
//! - **Cross-section boundary coherence** (manifest `selected` vs the selection
//!   inside `history/index.json`) requires parsing the history index, which is
//!   the runtime's concern; it is not checked here.

use std::collections::BTreeSet;

use liasse_ident::{DefinitionId, Digest};

use crate::archive::Archive;
use crate::error::{ArtifactError, MIMETYPE};
use crate::manifest::{Manifest, HISTORY_INDEX_PATH, LIASSE_JSON_PATH, STATE_PATH};

/// The direct archive leaf entries §19.5 requires `entries` to cover (in addition
/// to any present resource, history-segment/archive/definition, and blob leaves).
/// `manifest.json` is excluded — it cannot checksum itself — and nested child
/// artifacts under `modules/` are inventoried by `included_modules`, not `entries`.
const REQUIRED_ENTRIES: &[&str] = &["mimetype", LIASSE_JSON_PATH, STATE_PATH, HISTORY_INDEX_PATH];

/// The maximum recursive module-nesting depth [`Artifact::open`] verifies
/// before rejecting, so an adversarially deep artifact cannot overflow the
/// stack. Real compositions nest a handful deep at most.
const MAX_MODULE_DEPTH: usize = 64;

/// A verified, structurally well-formed `.liasse` artifact.
#[derive(Debug, Clone)]
pub struct Artifact {
    archive: Archive,
    manifest: Manifest,
}

impl Artifact {
    /// Open and fully verify a `.liasse` byte stream.
    pub fn open(bytes: &[u8]) -> Result<Self, ArtifactError> {
        Self::open_within(bytes, 0)
    }

    fn open_within(bytes: &[u8], depth: usize) -> Result<Self, ArtifactError> {
        if depth > MAX_MODULE_DEPTH {
            return Err(ArtifactError::ModuleNestingTooDeep {
                limit: MAX_MODULE_DEPTH,
            });
        }
        let archive = Archive::read(bytes)?;
        verify_mimetype(&archive)?;

        let manifest_bytes = archive
            .get("manifest.json")
            .ok_or(ArtifactError::ManifestMissing)?
            .data();
        let manifest = Manifest::parse(manifest_bytes)?;

        verify_entries_membership(&manifest)?;
        verify_entry_checksums(&archive, &manifest)?;
        verify_named_entry(&archive, &manifest.state.path, &manifest.state.sha256)?;
        verify_named_entry(&archive, &manifest.history.path, &manifest.history.sha256)?;
        require_entry(&archive, &manifest.definition.path)?;
        verify_modules(&archive, &manifest, depth)?;

        let artifact = Self { archive, manifest };
        // Annex D.5 internal self-consistency: the recorded definition identity
        // must equal the D.4 identity recomputed from the stored bytes; a stale
        // claimed identity over checksum-consistent bytes fails verification.
        artifact.verify_definition_identity()?;
        Ok(artifact)
    }

    /// The typed manifest.
    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The underlying archive.
    #[must_use]
    pub fn archive(&self) -> &Archive {
        &self.archive
    }

    /// The mimetype — always the §19.5 constant for an opened artifact.
    #[must_use]
    pub fn mimetype(&self) -> &'static str {
        MIMETYPE
    }

    /// The bytes of a named entry, if present.
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&[u8]> {
        self.archive.get(name).map(crate::archive::ArchiveEntry::data)
    }

    /// The canonical `liasse.json` definition bytes.
    #[must_use]
    pub fn liasse_json(&self) -> &[u8] {
        // Presence was proven by `require_entry` at open.
        self.entry(&self.manifest.definition.path).unwrap_or(&[])
    }

    /// The opaque selected-state section bytes (`state/current.cbor.zst`).
    #[must_use]
    pub fn state_section(&self) -> &[u8] {
        self.entry(&self.manifest.state.path).unwrap_or(&[])
    }

    /// The opaque history-index section bytes (`history/index.json`).
    #[must_use]
    pub fn history_index(&self) -> &[u8] {
        self.entry(&self.manifest.history.path).unwrap_or(&[])
    }

    /// The nested child module artifacts, by archive path. Each is itself a
    /// verified `.liasse` (proven at open); extract and reopen for its content.
    pub fn module_artifacts(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.manifest.included_modules.values().filter_map(|m| {
            self.archive
                .get(&m.artifact)
                .map(|entry| (entry.name(), entry.data()))
        })
    }

    /// The D.4 definition identity recomputed from the `liasse.json` bytes.
    #[must_use]
    pub fn definition_id(&self) -> DefinitionId {
        DefinitionId::of_canonical_bytes(self.liasse_json())
    }

    /// Check the manifest's declared `definition.identity` against the D.4
    /// identity recomputed from `liasse.json`. [`Artifact::open`] runs this as
    /// the mandatory Annex D.5 internal self-consistency step (item 21 pinned);
    /// it stays public for callers verifying a manifest they patched.
    pub fn verify_definition_identity(&self) -> Result<(), ArtifactError> {
        let computed = self.definition_id();
        if computed == self.manifest.definition.identity {
            Ok(())
        } else {
            Err(ArtifactError::DefinitionIdentityMismatch {
                declared: self.manifest.definition.identity,
                computed,
            })
        }
    }
}

fn verify_mimetype(archive: &Archive) -> Result<(), ArtifactError> {
    let entry = archive.get("mimetype").ok_or(ArtifactError::MimetypeMissing)?;
    match archive.first() {
        Some(first) if first.name() == "mimetype" => {}
        _ => return Err(ArtifactError::MimetypeNotFirst),
    }
    if !entry.is_stored() {
        return Err(ArtifactError::MimetypeCompressed);
    }
    let content = String::from_utf8_lossy(entry.data());
    if content.trim() == MIMETYPE {
        Ok(())
    } else {
        Err(ArtifactError::MimetypeContent {
            found: content.into_owned(),
        })
    }
}

/// Enforce the §19.5 `entries` membership rule (SPEC-ISSUES #33): `entries` covers
/// every required direct archive *leaf* — the four structural leaves here, plus any
/// present resource/history/blob section — and MUST NOT list `manifest.json`
/// (self-checksum) or a nested child artifact under `modules/` (which
/// `included_modules` inventories). The equality of a leaf's `entries` checksum and
/// any role-member checksum (`state`, `history`) is enforced transitively: each is
/// verified against the identical file bytes, so a divergence fails verification.
fn verify_entries_membership(manifest: &Manifest) -> Result<(), ArtifactError> {
    for required in REQUIRED_ENTRIES {
        if !manifest.entries.contains_key(*required) {
            return Err(ArtifactError::EntriesMissingRequired {
                path: (*required).to_owned(),
            });
        }
    }
    for path in manifest.entries.keys() {
        if path == "manifest.json" || path.starts_with("modules/") {
            return Err(ArtifactError::EntriesForbiddenMember { path: path.clone() });
        }
    }
    Ok(())
}

fn verify_entry_checksums(archive: &Archive, manifest: &Manifest) -> Result<(), ArtifactError> {
    for (path, checksum) in &manifest.entries {
        verify_named_entry(archive, path, &checksum.sha256)?;
    }
    Ok(())
}

fn verify_named_entry(archive: &Archive, path: &str, expected: &Digest) -> Result<(), ArtifactError> {
    let entry = require_entry(archive, path)?;
    let actual = Digest::of_bytes(entry.data());
    if actual == *expected {
        Ok(())
    } else {
        Err(ArtifactError::ChecksumMismatch {
            name: path.to_owned(),
            expected: *expected,
            actual,
        })
    }
}

fn require_entry<'a>(
    archive: &'a Archive,
    path: &str,
) -> Result<&'a crate::archive::ArchiveEntry, ArtifactError> {
    archive.get(path).ok_or_else(|| ArtifactError::MissingEntry {
        name: path.to_owned(),
    })
}

fn verify_modules(archive: &Archive, manifest: &Manifest, depth: usize) -> Result<(), ArtifactError> {
    let mut paths: BTreeSet<&str> = BTreeSet::new();

    for included in manifest.included_modules.values() {
        verify_named_entry(archive, &included.artifact, &included.sha256)?;
        paths.insert(included.artifact.as_str());
    }
    for mount in manifest.modules.values() {
        // A mounted child is required; verify presence even if not separately
        // inventoried in `included_modules`.
        require_entry(archive, &mount.artifact)?;
        paths.insert(mount.artifact.as_str());
    }

    // §19.8: verification is recursive — each nested artifact must itself open.
    for path in paths {
        let child = require_entry(archive, path)?;
        Artifact::open_within(child.data(), depth + 1)?;
    }
    Ok(())
}
