//! Build -> open round-trip and deterministic byte-identity (SPEC.md §4.2,
//! §19.5, §19.10).

mod common;

use liasse_artifact::{Artifact, MIMETYPE, STATE_PATH};

type Fallible = Result<(), Box<dyn std::error::Error>>;

#[test]
fn build_is_byte_deterministic() -> Fallible {
    // §4.2 / §19.10: the same inputs must produce byte-identical archives, so
    // the definition identity and round-trip are stable.
    let a = common::leaf_builder().build()?;
    let b = common::leaf_builder().build()?;
    assert_eq!(a, b);
    Ok(())
}

#[test]
fn opened_artifact_exposes_manifest_and_sections() -> Fallible {
    let bytes = common::leaf_bytes()?;
    let artifact = Artifact::open(&bytes)?;

    assert_eq!(artifact.mimetype(), MIMETYPE);
    assert_eq!(artifact.liasse_json(), common::definition().as_slice());
    assert_eq!(artifact.state_section(), b"OPAQUE-STATE-SECTION");

    let manifest = artifact.manifest();
    assert_eq!(manifest.instance.as_str(), "inst-root");
    assert_eq!(manifest.selected.lineage().as_str(), "lin-1");
    assert_eq!(manifest.selected.point().as_str(), "p1");
    assert_eq!(manifest.state.path, STATE_PATH);
    // §19.5: entries covers required entries but never manifest.json itself.
    assert!(manifest.entries.contains_key("mimetype"));
    assert!(manifest.entries.contains_key("liasse.json"));
    assert!(manifest.entries.contains_key(STATE_PATH));
    assert!(!manifest.entries.contains_key("manifest.json"));
    Ok(())
}

#[test]
fn manifest_bytes_round_trip_through_parse() -> Fallible {
    // A canonical manifest re-parses to an equal model: canonical encoding and
    // parsing are inverse over the closed format.
    let manifest = common::leaf_builder().manifest();
    let reparsed = liasse_artifact::Manifest::parse(&manifest.to_canonical_bytes())?;
    assert_eq!(manifest, reparsed);
    Ok(())
}

#[test]
fn definition_identity_is_d4_hash_of_liasse_json() -> Fallible {
    // Annex D.4: the definition identity is SHA-256 over the canonical
    // liasse.json bytes; the builder records exactly that, so the opt-in check
    // passes on a genuine artifact.
    let bytes = common::leaf_bytes()?;
    let artifact = Artifact::open(&bytes)?;
    artifact.verify_definition_identity()?;
    assert_eq!(
        artifact.manifest().definition.identity,
        liasse_ident::DefinitionId::of_canonical_bytes(&common::definition()),
    );
    Ok(())
}
