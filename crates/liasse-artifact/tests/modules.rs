//! Nested child-module artifacts and recursive verification (SPEC.md §19.5,
//! §19.8).

mod common;

use liasse_artifact::{Artifact, ArtifactError};
use liasse_ident::InstanceId;

type Fallible = Result<(), Box<dyn std::error::Error>>;

/// Build an independently valid child artifact.
fn child() -> Result<Vec<u8>, ArtifactError> {
    liasse_artifact::ArtifactBuilder::new(
        InstanceId::new("child-1"),
        common::point("clin-1", "cp1"),
        br#"{"$module":"t.feature@1.0.0","$liasse":1}"#.to_vec(),
        b"CHILD-STATE".to_vec(),
        br#"{"format":1,"selected":{"lineage":"clin-1","point":"cp1"}}"#.to_vec(),
    )
    .build()
}

fn parent_with_child() -> Result<Vec<u8>, ArtifactError> {
    let mut builder = common::leaf_builder();
    builder.module(
        "feature",
        InstanceId::new("child-1"),
        common::point("clin-1", "cp1"),
        child()?,
    );
    builder.build()
}

#[test]
fn child_is_embedded_inventoried_and_extractable() -> Fallible {
    let bytes = parent_with_child()?;
    let artifact = Artifact::open(&bytes)?;

    let manifest = artifact.manifest();
    let mount = manifest
        .modules
        .get("feature")
        .ok_or("missing mount `feature`")?;
    assert_eq!(mount.instance.as_str(), "child-1");
    assert_eq!(mount.artifact, "modules/child-1.liasse");
    assert!(manifest
        .included_modules
        .contains_key(&InstanceId::new("child-1")));

    // §19.5: extracting a modules/ entry yields an independently valid artifact.
    let (path, child_bytes) = artifact
        .module_artifacts()
        .next()
        .ok_or("no module artifact")?;
    assert_eq!(path, "modules/child-1.liasse");
    let extracted = Artifact::open(child_bytes)?;
    assert_eq!(extracted.manifest().instance.as_str(), "child-1");

    // The embedded child is byte-identical to an independent build of it.
    assert_eq!(child_bytes, child()?.as_slice());
    Ok(())
}

#[test]
fn tampered_child_bytes_fail_parent_checksum() -> Fallible {
    // Flip a byte inside the child artifact, leaving the parent's
    // included_modules checksum stale (Annex D.5, recursive §19.8).
    let mut entries = common::entries_of(&parent_with_child()?)?;
    for entry in &mut entries {
        if entry.0 == "modules/child-1.liasse"
            && let Some(last) = entry.1.last_mut()
        {
            *last ^= 0xFF;
        }
    }
    let tampered = common::repack(entries)?;
    match Artifact::open(&tampered) {
        Err(ArtifactError::ChecksumMismatch { name, .. }) => {
            assert_eq!(name, "modules/child-1.liasse");
        }
        other => return Err(format!("expected child checksum mismatch, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn invalid_child_artifact_fails_recursive_verification() -> Fallible {
    // §19.8: verification is recursive. A child inventoried with a matching
    // checksum but whose bytes are not a valid artifact must still be rejected.
    let mut builder = common::leaf_builder();
    builder.included_module(InstanceId::new("bad-1"), b"not a zip archive".to_vec());
    let bytes = builder.build()?;

    match Artifact::open(&bytes) {
        Err(ArtifactError::NotZip { .. }) => {}
        other => return Err(format!("expected recursive verification failure, got {other:?}").into()),
    }
    Ok(())
}
