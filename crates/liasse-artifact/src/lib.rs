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
//! Item 21 is pinned (SPEC.md §19.11/Annex D.5): verification establishes byte
//! integrity plus internal self-consistency and nothing else, so
//! [`Artifact::open`] mandatorily rejects a stale `definition.identity` while
//! **tolerating** unknown extra archive entries (forward compatibility). The
//! Annex D.9 physical package digest and detached-signature verification are an
//! honest follow-on hole: not built yet, and nothing here decides trust — that
//! stays the host application's call. Same-version republish and downgrade are
//! pinned by §20/Annex E.1 (item 22): the [`ContractRule`] variants carry which
//! rule governs, and the runtime applies the non-narrowing gate to same-version
//! republishes too. Version prerelease/build metadata (item 26) is rejected by
//! the strict three-component [`Version`].

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
