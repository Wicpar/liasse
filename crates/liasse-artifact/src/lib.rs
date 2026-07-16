//! Artifacts: the portable `.liasse` ZIP64 archive — manifest, checksums,
//! packing and unpacking, and package compatibility (SPEC.md §4.1, §19.5,
//! Annex D, Annex E).
//!
//! # Layers
//!
//! - [`Archive`] / [`ArchiveBuilder`] — the ZIP64 container. Reading rejects
//!   path traversal and duplicate names; writing is byte-deterministic.
//! - [`Manifest`] — the typed, closed format-1 `manifest.json` (§19.5): the
//!   represented instance, selected point, direct-module composition, included
//!   child artifacts, and per-entry checksums, all over `liasse-ident` digests
//!   and identities.
//! - [`Artifact`] / [`ArtifactBuilder`] — [open-and-verify](Artifact::open) an
//!   artifact (the recursive §19.8 verification) and [compose](ArtifactBuilder)
//!   one from parts.
//! - [`CompatibilityDecision`] — the Annex E version-relationship decision the
//!   runtime consumes ([`PackageIdentity`], [`Version`]).
//!
//! # Boundary
//!
//! State and history section contents are **opaque bytes** here — verified by
//! checksum and handed back verbatim. Their CBOR/Zstandard decoding, the
//! `history/index.json` structure, and the §19 merge/reconcile semantics live
//! in the runtime above. This crate provides faithful container, manifest, and
//! compatibility primitives, nothing more.
//!
//! # Documented spec-gap choices
//!
//! Where the spec leaves behavior unpinned this crate does not silently pick a
//! side: a stale `definition.identity` and unknown extra entries (SPEC-ISSUES
//! item 21) are not rejected by [`Artifact::open`] — the former is offered as
//! the opt-in [`Artifact::verify_definition_identity`]. Same-version republish
//! and shape-compatible downgrade (item 22) are surfaced as distinct
//! [`ContractRule`] variants rather than pre-decided. Version prerelease/build
//! metadata (item 26) is rejected by the strict three-component [`Version`].

mod archive;
mod artifact;
mod build;
mod canon;
mod compat;
mod error;
mod manifest;
mod raw;
mod version;

pub use archive::{Archive, ArchiveBuilder, ArchiveEntry};
pub use artifact::Artifact;
pub use build::ArtifactBuilder;
pub use compat::{CompatibilityDecision, ContractRule, UpdateRelation};
pub use error::{ArtifactError, MIMETYPE};
pub use manifest::{
    DefinitionRef, EntryChecksum, EntryRef, IncludedModule, Manifest, MountRef, HISTORY_INDEX_PATH,
    LIASSE_JSON_PATH, STATE_PATH,
};
pub use version::{PackageIdentity, PackageName, Version};
