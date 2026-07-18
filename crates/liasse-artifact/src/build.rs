//! Composing a `.liasse` artifact from its parts (SPEC.md §4.2, §19.5, §19.7,
//! Annex D.4/D.5).
//!
//! [`ArtifactBuilder`] assembles the container from opaque section bytes the
//! runtime layer owns: the canonical `liasse.json`, the selected state section,
//! the history index, path-addressed extra sections (resources, history
//! segments/archives, blobs), and nested child artifacts. It computes the D.4
//! definition identity and every D.5 checksum, writes the closed format-1
//! manifest, and emits the deterministic archive.
//!
//! **Determinism.** Given the same inputs the output is byte-identical: the
//! manifest is canonical JSON, and [`crate::archive::ArchiveBuilder`] fixes
//! entry order (mimetype first, then name order), compression (STORED),
//! timestamp, and permissions. This is what the definition identity (§4.2) and
//! the round-trip guarantee (§19.10) require.
//!
//! **Media types** are not pinned by the spec (§19.5 records a media type per
//! entry but fixes no vocabulary); the constants below are the builder's stable
//! choice. `entries` covers every required direct archive *leaf* — the four
//! structural leaves plus any resource/history/blob section — and excludes both
//! `manifest.json` (self-checksum) and the nested module artifacts, which the
//! dedicated `included_modules` inventory covers (§19.5, the exhaustive membership
//! rule pinned by SPEC-ISSUES #33).

use std::collections::BTreeMap;

use liasse_ident::{DefinitionId, Digest, HistoryPoint, InstanceId};

use crate::archive::ArchiveBuilder;
use crate::error::{ArtifactError, MIMETYPE};
use crate::manifest::{
    DefinitionRef, EntryChecksum, EntryRef, IncludedModule, Manifest, MountRef, HISTORY_INDEX_PATH,
    LIASSE_JSON_PATH, STATE_PATH,
};

/// Media type recorded for the `mimetype` entry.
const MEDIA_MIMETYPE: &str = "text/plain";
/// Media type recorded for canonical-JSON entries (`liasse.json`, history index).
const MEDIA_JSON: &str = "application/json";
/// Media type recorded for the CBOR+Zstandard state section.
const MEDIA_STATE: &str = "application/cbor+zstd";

/// One path-addressed extra section (a resource, history segment/archive, or
/// blob) included verbatim, with its own media type.
struct Section {
    path: String,
    media: String,
    bytes: Vec<u8>,
}

/// One nested direct child module artifact.
struct ChildModule {
    incarnation: InstanceId,
    bytes: Vec<u8>,
    /// The mount name and selection when the child is a *current* mount; `None`
    /// for a child required only by retained history.
    mount: Option<(String, HistoryPoint)>,
}

impl ChildModule {
    fn artifact_path(&self) -> String {
        format!("modules/{}.liasse", self.incarnation.as_str())
    }
}

/// A builder for a `.liasse` artifact.
pub struct ArtifactBuilder {
    instance: InstanceId,
    selected: HistoryPoint,
    definition: Vec<u8>,
    state: Vec<u8>,
    history_index: Vec<u8>,
    sections: Vec<Section>,
    children: Vec<ChildModule>,
}

impl ArtifactBuilder {
    /// Start a builder for one instance and selected point, with the required
    /// definition, state, and history-index sections.
    #[must_use]
    pub fn new(
        instance: InstanceId,
        selected: HistoryPoint,
        definition: Vec<u8>,
        state: Vec<u8>,
        history_index: Vec<u8>,
    ) -> Self {
        Self {
            instance,
            selected,
            definition,
            state,
            history_index,
            sections: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Add a path-addressed extra section (a resource under `resources/`, a
    /// history segment/archive under `history/`, or a blob under `blobs/`).
    pub fn section(
        &mut self,
        path: impl Into<String>,
        media: impl Into<String>,
        bytes: Vec<u8>,
    ) -> &mut Self {
        self.sections.push(Section {
            path: path.into(),
            media: media.into(),
            bytes,
        });
        self
    }

    /// Add a currently-mounted direct child module artifact.
    pub fn module(
        &mut self,
        mount_name: impl Into<String>,
        incarnation: InstanceId,
        selected: HistoryPoint,
        artifact_bytes: Vec<u8>,
    ) -> &mut Self {
        self.children.push(ChildModule {
            incarnation,
            bytes: artifact_bytes,
            mount: Some((mount_name.into(), selected)),
        });
        self
    }

    /// Add a child module artifact required only by retained history, absent
    /// from the current composition (§19.5 `included_modules`).
    pub fn included_module(
        &mut self,
        incarnation: InstanceId,
        artifact_bytes: Vec<u8>,
    ) -> &mut Self {
        self.children.push(ChildModule {
            incarnation,
            bytes: artifact_bytes,
            mount: None,
        });
        self
    }

    /// Compute the manifest for the accumulated parts without serializing the
    /// archive (useful for inspection and testing).
    #[must_use]
    pub fn manifest(&self) -> Manifest {
        let mut entries = BTreeMap::new();
        entries.insert(
            "mimetype".to_owned(),
            checksum(MEDIA_MIMETYPE, MIMETYPE.as_bytes()),
        );
        entries.insert(
            LIASSE_JSON_PATH.to_owned(),
            checksum(MEDIA_JSON, &self.definition),
        );
        entries.insert(STATE_PATH.to_owned(), checksum(MEDIA_STATE, &self.state));
        entries.insert(
            HISTORY_INDEX_PATH.to_owned(),
            checksum(MEDIA_JSON, &self.history_index),
        );
        for section in &self.sections {
            entries.insert(section.path.clone(), checksum(&section.media, &section.bytes));
        }

        let mut modules = BTreeMap::new();
        let mut included_modules = BTreeMap::new();
        for child in &self.children {
            let path = child.artifact_path();
            included_modules.insert(
                child.incarnation.clone(),
                IncludedModule {
                    artifact: path.clone(),
                    sha256: Digest::of_bytes(&child.bytes),
                },
            );
            if let Some((mount_name, selected)) = &child.mount {
                modules.insert(
                    mount_name.clone(),
                    MountRef {
                        instance: child.incarnation.clone(),
                        artifact: path,
                        selected: selected.clone(),
                    },
                );
            }
        }

        Manifest {
            instance: self.instance.clone(),
            selected: self.selected.clone(),
            definition: DefinitionRef {
                identity: DefinitionId::of_canonical_bytes(&self.definition),
                path: LIASSE_JSON_PATH.to_owned(),
            },
            state: EntryRef {
                path: STATE_PATH.to_owned(),
                sha256: Digest::of_bytes(&self.state),
            },
            history: EntryRef {
                path: HISTORY_INDEX_PATH.to_owned(),
                sha256: Digest::of_bytes(&self.history_index),
            },
            modules,
            included_modules,
            entries,
        }
    }

    /// Serialize the complete deterministic `.liasse` archive.
    pub fn build(self) -> Result<Vec<u8>, ArtifactError> {
        let manifest = self.manifest();
        let mut archive = ArchiveBuilder::new();
        archive.add("mimetype", MIMETYPE.as_bytes().to_vec());
        archive.add("manifest.json", manifest.to_canonical_bytes());
        archive.add(LIASSE_JSON_PATH, self.definition);
        archive.add(STATE_PATH, self.state);
        archive.add(HISTORY_INDEX_PATH, self.history_index);
        for section in self.sections {
            archive.add(section.path, section.bytes);
        }
        for child in self.children {
            archive.add(child.artifact_path(), child.bytes);
        }
        archive.finish()
    }
}

fn checksum(media: &str, bytes: &[u8]) -> EntryChecksum {
    EntryChecksum {
        media: media.to_owned(),
        sha256: Digest::of_bytes(bytes),
    }
}
