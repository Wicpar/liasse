// Each integration-test binary includes this module but uses only some
// helpers; the unused-per-binary ones are not dead across the suite.
#![allow(dead_code)]

//! Shared fixtures for the `liasse-artifact` integration tests.
//!
//! State / history / definition bytes are deliberately opaque stand-ins: this
//! crate treats them as sections with digests, so the tests only need stable
//! bytes, not real CBOR or a real definition.

use liasse_artifact::{Archive, ArchiveBuilder, ArtifactBuilder, ArtifactError};
use liasse_ident::{HistoryPoint, InstanceId, LineageId, PointId};

/// A history point from two opaque tokens.
pub fn point(lineage: &str, point: &str) -> HistoryPoint {
    HistoryPoint::new(LineageId::new(lineage), PointId::new(point))
}

/// The sample definition bytes (their content is opaque to this crate).
pub fn definition() -> Vec<u8> {
    br#"{"$app":"t.hist@1.0.0","$liasse":1}"#.to_vec()
}

/// A fresh builder for a single-instance leaf artifact (no children).
pub fn leaf_builder() -> ArtifactBuilder {
    ArtifactBuilder::new(
        InstanceId::new("inst-root"),
        point("lin-1", "p1"),
        definition(),
        b"OPAQUE-STATE-SECTION".to_vec(),
        br#"{"format":1,"selected":{"lineage":"lin-1","point":"p1"}}"#.to_vec(),
    )
}

/// The serialized bytes of the sample leaf artifact.
pub fn leaf_bytes() -> Result<Vec<u8>, ArtifactError> {
    leaf_builder().build()
}

/// Read an archive's entries as `(name, bytes)` in stored order.
pub fn entries_of(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, ArtifactError> {
    let archive = Archive::read(bytes)?;
    Ok(archive
        .entries()
        .iter()
        .map(|entry| (entry.name().to_owned(), entry.data().to_vec()))
        .collect())
}

/// Re-pack a set of `(name, bytes)` entries into a deterministic archive.
pub fn repack(entries: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, ArtifactError> {
    let mut builder = ArchiveBuilder::new();
    for (name, bytes) in entries {
        builder.add(name, bytes);
    }
    builder.finish()
}

/// Re-pack the sample artifact with one entry's bytes replaced.
pub fn repack_replacing(target: &str, bytes: Vec<u8>) -> Result<Vec<u8>, ArtifactError> {
    let mut entries = entries_of(&leaf_bytes()?)?;
    for entry in &mut entries {
        if entry.0 == target {
            entry.1 = bytes.clone();
        }
    }
    repack(entries)
}

/// Re-pack the sample artifact with one entry removed.
pub fn repack_removing(target: &str) -> Result<Vec<u8>, ArtifactError> {
    let entries = entries_of(&leaf_bytes()?)?
        .into_iter()
        .filter(|(name, _)| name != target)
        .collect();
    repack(entries)
}
