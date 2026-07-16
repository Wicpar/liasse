//! The typed `manifest.json` model (SPEC.md §19.5, Annex D.5).
//!
//! A parsed [`Manifest`] is proof the JSON matched the closed format-1
//! structure: `format` is 1, every required member is present and well-typed,
//! and no member outside the vocabulary appears ("Additional members are
//! invalid for format version 1", §19.5). `format` is not a field — a
//! [`Manifest`] is *always* format 1 — so an unsupported or missing `format`
//! can only be a parse rejection, never a representable value.
//!
//! Digests are [`liasse_ident::Digest`]; identities are the opaque
//! [`liasse_ident`] tokens. The manifest carries content checksums; history
//! ancestry and point identity are represented explicitly elsewhere (§19.6).

mod parse;

use std::collections::BTreeMap;

use liasse_ident::{DefinitionId, Digest, HistoryPoint, InstanceId};

use crate::canon::Json;

/// The archive path of the canonical definition entry (§19.5).
pub const LIASSE_JSON_PATH: &str = "liasse.json";
/// The archive path of the selected state entry (§19.5).
pub const STATE_PATH: &str = "state/current.cbor.zst";
/// The archive path of the history index entry (§19.5).
pub const HISTORY_INDEX_PATH: &str = "history/index.json";

/// A reference to the definition entry and its D.4 identity (§19.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionRef {
    /// The D.4 definition identity declared for `liasse.json`.
    pub identity: DefinitionId,
    /// The archive path of the definition entry.
    pub path: String,
}

/// A path-plus-checksum reference to a required entry (§19.5, D.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRef {
    /// The archive path.
    pub path: String,
    /// The SHA-256 of the exact entry bytes.
    pub sha256: Digest,
}

/// The recorded media type and checksum of one required archive entry (§19.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryChecksum {
    /// The uncompressed media type (D.5).
    pub media: String,
    /// The SHA-256 of the exact entry bytes.
    pub sha256: Digest,
}

/// A currently-mounted direct child module (§19.5 `modules`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountRef {
    /// The child instance incarnation.
    pub instance: InstanceId,
    /// The nested child artifact path (`modules/<incarnation>.liasse`).
    pub artifact: String,
    /// The selected child lineage and point.
    pub selected: HistoryPoint,
}

/// A direct child artifact required by the export (§19.5 `included_modules`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludedModule {
    /// The nested child artifact path.
    pub artifact: String,
    /// The SHA-256 of the exact child `.liasse` bytes.
    pub sha256: Digest,
}

/// The parsed `manifest.json` (§19.5). Always format 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The represented instance incarnation.
    pub instance: InstanceId,
    /// The selected lineage and point.
    pub selected: HistoryPoint,
    /// The definition entry and its identity.
    pub definition: DefinitionRef,
    /// The selected state entry.
    pub state: EntryRef,
    /// The history index entry.
    pub history: EntryRef,
    /// Selected direct mounts by mount name.
    pub modules: BTreeMap<String, MountRef>,
    /// Every direct child artifact required, by child incarnation.
    pub included_modules: BTreeMap<InstanceId, IncludedModule>,
    /// Every required direct archive entry other than `manifest.json`, by path.
    pub entries: BTreeMap<String, EntryChecksum>,
}

/// The closed set of top-level format-1 members (§19.5).
const TOP_MEMBERS: &[&str] = &[
    "format",
    "instance",
    "selected",
    "definition",
    "state",
    "history",
    "modules",
    "included_modules",
    "entries",
];

impl Manifest {
    /// Encode to canonical strict-JSON bytes (§19.5, D.5). Member names are in
    /// Unicode-scalar order; the same [`Manifest`] always encodes to the same
    /// bytes.
    #[must_use]
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        self.to_json().to_canonical_bytes()
    }

    fn to_json(&self) -> Json {
        Json::object([
            ("format".to_owned(), Json::Int(1)),
            ("instance".to_owned(), Json::str(self.instance.as_str())),
            ("selected".to_owned(), point_json(&self.selected)),
            (
                "definition".to_owned(),
                Json::object([
                    (
                        "identity".to_owned(),
                        Json::str(self.definition.identity.to_canonical_text()),
                    ),
                    ("path".to_owned(), Json::str(&self.definition.path)),
                ]),
            ),
            ("state".to_owned(), entry_ref_json(&self.state)),
            ("history".to_owned(), entry_ref_json(&self.history)),
            (
                "modules".to_owned(),
                Json::object(self.modules.iter().map(|(name, mount)| {
                    (
                        name.clone(),
                        Json::object([
                            ("instance".to_owned(), Json::str(mount.instance.as_str())),
                            ("artifact".to_owned(), Json::str(&mount.artifact)),
                            ("selected".to_owned(), point_json(&mount.selected)),
                        ]),
                    )
                })),
            ),
            (
                "included_modules".to_owned(),
                Json::object(self.included_modules.iter().map(|(inc, m)| {
                    (
                        inc.as_str().to_owned(),
                        Json::object([
                            ("artifact".to_owned(), Json::str(&m.artifact)),
                            ("sha256".to_owned(), Json::str(m.sha256.to_canonical_text())),
                        ]),
                    )
                })),
            ),
            (
                "entries".to_owned(),
                Json::object(self.entries.iter().map(|(path, e)| {
                    (
                        path.clone(),
                        Json::object([
                            ("media".to_owned(), Json::str(&e.media)),
                            ("sha256".to_owned(), Json::str(e.sha256.to_canonical_text())),
                        ]),
                    )
                })),
            ),
        ])
    }
}

fn point_json(point: &HistoryPoint) -> Json {
    Json::object([
        ("lineage".to_owned(), Json::str(point.lineage().as_str())),
        ("point".to_owned(), Json::str(point.point().as_str())),
    ])
}

fn entry_ref_json(entry: &EntryRef) -> Json {
    Json::object([
        ("path".to_owned(), Json::str(&entry.path)),
        ("sha256".to_owned(), Json::str(entry.sha256.to_canonical_text())),
    ])
}
