//! Canonical identity, observable paths, and integrity (SPEC.md Annex D).
//!
//! This crate turns the raw text and opaque tokens of Annex D into semantic
//! types, layered on the canonical values of `liasse-value`:
//!
//! - [`KeyComponent`] / [`KeyText`] — canonical scalar key text and the
//!   escaped, `:`-joined full-key text of D.2 (seed member names, key path
//!   segments, canonical exports).
//! - [`NameSegment`], [`PathSegment`], [`CanonicalPath`], [`RawSegment`] — the
//!   D.3 display-path model: typed segments with a pinned display form and a
//!   fallible string parse.
//! - [`InstanceId`], [`RowIncarnation`], [`LineageId`], [`PointId`],
//!   [`TransactionId`], [`RangeId`], [`RowIdentity`], [`InstanceIdentity`],
//!   [`HistoryPoint`] — the D.1/D.5 identity model, with incarnation-based
//!   equality that keeps identity stable across rekey and rebind.
//! - [`Digest`] / [`DefinitionId`] — the D.4/D.5/D.7 SHA-256 integrity scheme,
//!   with the canonical `sha256:<hex>` text.
//!
//! # Parse, don't validate
//!
//! A constructed [`CanonicalPath`], [`KeyText`], or [`Digest`] is proof of
//! conformance; the only fallible boundaries are the `parse` constructors that
//! ingest untrusted text.
//!
//! # Documented spec-gap choices
//!
//! Where Annex D leaves observable behavior unpinned this crate makes the
//! least-surprising choice and cites the SPEC-ISSUES item: SHA-256 input hex
//! case (item 20 by analogy, [`Digest::parse`]) and point-id aliasing across
//! lineages (item 21, [`HistoryPoint`]).

mod digest;
mod error;
mod escape;
mod identity;
mod key;
mod path;

pub use digest::{DefinitionId, Digest};
pub use error::IdentError;
pub use identity::{
    HistoryPoint, InstanceId, InstanceIdentity, LineageId, PointId, RangeId, RowIdentity,
    RowIncarnation, TransactionId,
};
pub use key::{KeyComponent, KeyText};
pub use path::{CanonicalPath, NameSegment, PathSegment, RawSegment};
