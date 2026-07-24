#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Package-from-blob decode (§13.10, §20.1): `decode_package_from_blob` opens and
//! integrity-verifies a `.liasse` container, then hands back the UTF-8 definition
//! text (head `$model` + `$migrations`) and its D.4 identity — the boundary a
//! host-privileged `module.install`/`module.update` mutation decodes. Malformed or
//! non-UTF-8 inputs fail LOUDLY as typed errors, never a partial decode.

mod common;

use liasse_artifact::{decode_package_from_blob, Artifact, ArtifactBuilder, ArtifactError};
use liasse_ident::{DefinitionId, HistoryPoint, InstanceId, LineageId, PointId};

fn point(lineage: &str, id: &str) -> HistoryPoint {
    HistoryPoint::new(LineageId::new(lineage), PointId::new(id))
}

/// Build a leaf `.liasse` carrying `definition` as its `liasse.json` bytes.
fn artifact_with(definition: &[u8]) -> Vec<u8> {
    ArtifactBuilder::new(
        InstanceId::new("pkg-inst"),
        point("lin-1", "p1"),
        definition.to_vec(),
        b"OPAQUE-STATE".to_vec(),
        br#"{"format":1,"selected":{"lineage":"lin-1","point":"p1"}}"#.to_vec(),
    )
    .build()
    .expect("the artifact builds")
}

#[test]
fn decodes_the_definition_and_d4_identity_from_a_valid_blob() {
    let definition = br#"{"$module":"acme.sales@1.2.0","$liasse":1,"$model":{}}"#;
    let bytes = artifact_with(definition);

    let decoded = decode_package_from_blob(&bytes).expect("a valid blob decodes");

    assert_eq!(
        decoded.definition().as_bytes(),
        &definition[..],
        "the decoded definition text is the verbatim liasse.json"
    );
    assert_eq!(
        decoded.definition_id(),
        &DefinitionId::of_canonical_bytes(definition),
        "the decoded identity is the D.4 identity of the definition bytes"
    );
    // The artifact-layer decode agrees with the container's own `definition_id`.
    let artifact = Artifact::open(&bytes).expect("opens");
    assert_eq!(decoded.definition_id(), &artifact.definition_id());
}

#[test]
fn a_non_liasse_byte_stream_fails_loudly() {
    let error = decode_package_from_blob(b"not a zip at all").expect_err("garbage is refused");
    assert!(
        matches!(error, ArtifactError::NotZip { .. }),
        "a non-container blob is a loud NotZip, got {error:?}"
    );
}

#[test]
fn a_tampered_definition_section_fails_loudly() {
    // Corrupt the definition bytes AFTER the manifest fixed its checksum: opening
    // must reject the checksum mismatch rather than decode a lying definition.
    let mut entries = common::entries_of(&artifact_with(
        br#"{"$module":"acme.sales@1.0.0","$liasse":1}"#,
    ))
    .expect("read entries");
    for entry in &mut entries {
        if entry.0 == "liasse.json" {
            entry.1 = br#"{"$module":"evil.pkg@9.9.9","$liasse":1}"#.to_vec();
        }
    }
    let tampered = common::repack(entries).expect("repack");

    let error = decode_package_from_blob(&tampered).expect_err("a tampered definition is refused");
    assert!(
        matches!(error, ArtifactError::ChecksumMismatch { .. } | ArtifactError::DefinitionIdentityMismatch { .. }),
        "a tampered definition fails integrity verification, got {error:?}"
    );
}

#[test]
fn a_non_utf8_definition_fails_loudly() {
    // A definition section that is valid bytes for the container but not UTF-8 text.
    let bytes = artifact_with(&[0xff, 0xfe, 0x00, 0x9f]);
    let error = decode_package_from_blob(&bytes).expect_err("non-UTF-8 is refused");
    assert!(
        matches!(error, ArtifactError::DefinitionNotUtf8 { .. }),
        "a non-UTF-8 definition is a loud DefinitionNotUtf8, got {error:?}"
    );
}
