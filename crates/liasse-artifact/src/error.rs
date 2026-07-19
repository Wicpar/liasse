//! Typed artifact errors (SPEC.md §4.1, §19.5, Annex D.5, Annex E).
//!
//! Every failure names the offending archive entry or manifest member. These
//! are the categories the byte-level tamper corpus asserts against (tests/04,
//! tests/19): a corrupted section fails as [`ArtifactError::ChecksumMismatch`],
//! a stripped section as [`ArtifactError::MissingEntry`], a duplicated one as
//! [`ArtifactError::DuplicateEntry`], and so on. The runtime maps each to its
//! load/import diagnostic; this crate does not render diagnostics itself
//! (mirroring `liasse-store`'s `StoreError`).

use liasse_ident::{DefinitionId, Digest, IdentError};

/// The exact byte content the `mimetype` entry must carry (§19.5).
pub const MIMETYPE: &str = "application/vnd.liasse+zip";

/// Anything that makes a byte stream fail to be a well-formed `.liasse`
/// artifact, or a manifest fail to verify against its archive.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    /// The byte stream is not a readable ZIP container at all (§4.1).
    #[error("not a readable ZIP archive: {detail}")]
    NotZip {
        /// Underlying reader detail.
        detail: String,
    },

    /// The container writer failed while producing an archive (§4.1). Building
    /// into memory does not fail in practice; this exists so no build path
    /// panics.
    #[error("failed to write ZIP container: {detail}")]
    ContainerWrite {
        /// Underlying writer detail.
        detail: String,
    },

    /// An entry name escapes the archive root (`..`, absolute, or a drive
    /// prefix). Zip-slip; never part of the §19.5 structure.
    #[error("archive entry `{name}` escapes the artifact root")]
    EntryOutsideRoot {
        /// The offending raw entry name.
        name: String,
    },

    /// Two archive entries share one name. Annex D.5 requires every referenced
    /// entry to exist *exactly once*; a duplicate is a parser-differential
    /// vector.
    #[error("archive entry `{name}` occurs more than once")]
    DuplicateEntry {
        /// The duplicated entry name.
        name: String,
    },

    /// The `mimetype` entry is absent (§19.5).
    #[error("required `mimetype` entry is missing")]
    MimetypeMissing,

    /// The `mimetype` entry is not the first archive member (§19.5 container
    /// convention).
    #[error("`mimetype` entry is not the first archive member")]
    MimetypeNotFirst,

    /// The `mimetype` entry is compressed; the convention stores it verbatim.
    #[error("`mimetype` entry must be stored uncompressed")]
    MimetypeCompressed,

    /// The `mimetype` entry does not carry the exact §19.5 content.
    #[error("`mimetype` entry does not contain `{MIMETYPE}` (found `{found}`)")]
    MimetypeContent {
        /// The bytes actually found, lossily decoded for the message.
        found: String,
    },

    /// A manifest-referenced entry is absent from the archive (Annex D.5).
    #[error("referenced entry `{name}` does not exist")]
    MissingEntry {
        /// The archive path the manifest referenced.
        name: String,
    },

    /// A referenced entry's bytes do not match the recorded checksum
    /// (Annex D.5).
    #[error(
        "checksum mismatch for `{name}`: manifest records {expected}, entry hashes to {actual}",
        expected = .expected.to_canonical_text(),
        actual = .actual.to_canonical_text(),
    )]
    ChecksumMismatch {
        /// The archive path whose bytes disagree with the manifest.
        name: String,
        /// The digest the manifest recorded.
        expected: Digest,
        /// The digest of the entry's actual bytes.
        actual: Digest,
    },

    /// `manifest.json` is absent (§19.5).
    #[error("required `manifest.json` entry is missing")]
    ManifestMissing,

    /// `manifest.json` is not readable strict JSON (§19.5, D.5).
    #[error("`manifest.json` is not valid strict JSON: {detail}")]
    ManifestJson {
        /// Decoder detail.
        detail: String,
    },

    /// A required manifest member is absent (§19.5).
    #[error("`manifest.json` is missing required member `{member}`")]
    ManifestMissingMember {
        /// The absent member's canonical name.
        member: &'static str,
    },

    /// A manifest member has the wrong shape or an unparseable value (§19.5).
    #[error("`manifest.json` member `{member}` is invalid: {detail}")]
    ManifestBadValue {
        /// The offending member's path within the manifest.
        member: String,
        /// Why it is invalid.
        detail: String,
    },

    /// A member outside the closed format-1 vocabulary is present (§19.5:
    /// "Additional members are invalid for format version 1").
    #[error("`manifest.json` has member `{name}`, invalid for format 1")]
    ManifestUnknownMember {
        /// The unexpected member's path within the manifest.
        name: String,
    },

    /// The manifest `format` is not the supported version 1 (§19.5).
    #[error("unsupported manifest `format` {found}; only format 1 is supported")]
    ManifestFormatUnsupported {
        /// The value found in the `format` member.
        found: i64,
    },

    /// `entries` omits a required direct archive leaf it must cover (§19.5:
    /// `mimetype`, `liasse.json`, `state/current.cbor.zst`, `history/index.json`).
    /// Its uncompressed media type is then recorded nowhere, which D.5 requires for
    /// every required non-manifest entry.
    #[error("manifest `entries` is missing required entry `{path}` (§19.5)")]
    EntriesMissingRequired {
        /// The required archive path absent from `entries`.
        path: String,
    },

    /// `entries` lists a member §19.5 forbids: `manifest.json` (which cannot
    /// checksum itself) or a nested child artifact under `modules/` (inventoried by
    /// `included_modules`, not `entries`).
    #[error("manifest `entries` must not list `{path}` (§19.5)")]
    EntriesForbiddenMember {
        /// The offending archive path listed in `entries`.
        path: String,
    },

    /// A role member (`definition`, `state`, or `history`) names an archive path
    /// other than the §19.5 canonical literal it is fixed to (`liasse.json`,
    /// `state/current.cbor.zst`, `history/index.json`). §19.5 pins each role `path`
    /// to its literal so a role member can only name the entries-covered leaf; that
    /// is what makes "Where a covered entry's checksum also appears in a role
    /// member, the two MUST be equal" real (the role checksum and its `entries`
    /// coverage are then the same bytes). A repointed role path would let the
    /// consumed section (`state`/`history`/`liasse.json`) silently diverge from what
    /// `entries` certifies, so it is rejected before any role member is trusted.
    #[error("manifest `{role}` role path `{path}` is not the fixed §19.5 canonical archive path")]
    NonCanonicalRolePath {
        /// The role whose path is non-canonical (`definition`, `state`, `history`).
        role: &'static str,
        /// The non-canonical archive path the manifest declared for that role.
        path: String,
    },

    /// The manifest's declared definition identity disagrees with the D.4
    /// identity recomputed from `liasse.json` — the mandatory Annex D.5
    /// internal self-consistency check `open` runs (see
    /// [`crate::Artifact::verify_definition_identity`]).
    #[error(
        "definition identity mismatch: manifest declares {declared}, `liasse.json` hashes to {computed}",
        declared = .declared.to_canonical_text(),
        computed = .computed.to_canonical_text(),
    )]
    DefinitionIdentityMismatch {
        /// The identity the manifest declared.
        declared: DefinitionId,
        /// The identity recomputed from the definition bytes.
        computed: DefinitionId,
    },

    /// Nested module artifacts are nested deeper than the verification limit.
    /// A bound on recursive [`crate::Artifact::open`] so an adversarially deep
    /// artifact cannot overflow the stack (§19.5 recursive containment).
    #[error("module nesting exceeds the verification limit of {limit}")]
    ModuleNestingTooDeep {
        /// The maximum supported nesting depth.
        limit: usize,
    },

    /// A `name@version` package identity failed the §2.5/§4.3/E.1 grammar.
    #[error("invalid package identity: {detail}")]
    PackageIdentity {
        /// Why the identity text was rejected.
        detail: String,
    },

    /// A digest string in the manifest failed the Annex D `sha256:` grammar.
    #[error("invalid digest in manifest: {0}")]
    Digest(#[from] IdentError),
}
