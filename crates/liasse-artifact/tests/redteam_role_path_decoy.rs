//! RED-TEAM probe of the freshly-landed #21 self-consistency verification
//! (SPEC.md §19.5, §19.10/§19.11, Annex D.4/D.5). Item 21 pins verification as
//! "byte integrity plus internal self-consistency and nothing else". This
//! battery attacks a self-consistency hole the pin misses: the manifest's three
//! *role paths* — `definition.path`, `state.path`, `history.path` — are
//! free-form strings that `Artifact::open` never constrains to the fixed
//! canonical archive paths the §19.5 structure mandates
//! (`liasse.json`, `state/current.cbor.zst`, `history/index.json`) and that
//! `entries` is required to cover.
//!
//! Consequence: a role member can point at a DECOY archive entry that is NOT the
//! entries-covered leaf, while `entries` still faithfully covers the genuine
//! canonical leaf. Every checksum verifies (each against its own file), so open
//! ACCEPTS — yet the section the runtime consumes (`liasse_json()`,
//! `state_section()`, `history_index()`) is the attacker's decoy, and for the
//! definition the artifact's D.4 IDENTITY is computed over the decoy. This
//! defeats §19.5 "Where a covered entry's checksum also appears in a role member
//! (`state`, `history`), the two MUST be equal": the role member and its
//! entries coverage now describe different files, so the equality is vacuous.
//!
//! Every expectation is deducible from SPEC.md §19.5 alone: the required
//! manifest structure fixes each role `path` to a literal (`liasse.json`,
//! `state/current.cbor.zst`, `history/index.json`) — angle-bracket placeholders
//! are used for every free value, these three are not — and `entries` "covers
//! every required direct archive *leaf* entry" naming exactly those paths.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use liasse_artifact::{Artifact, ArtifactError, HISTORY_INDEX_PATH, LIASSE_JSON_PATH, STATE_PATH};
use liasse_ident::{DefinitionId, Digest};

type Fallible = Result<(), Box<dyn std::error::Error>>;

/// Repack the sample leaf artifact with a mutated manifest and any number of
/// extra `(path, bytes)` archive entries appended (the decoy leaves).
fn repack_with(
    manifest: &liasse_artifact::Manifest,
    extra: &[(&str, Vec<u8>)],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let canonical = manifest.to_canonical_bytes();
    let mut entries = common::entries_of(&common::leaf_bytes()?)?;
    for entry in &mut entries {
        if entry.0 == "manifest.json" {
            entry.1 = canonical.clone();
        }
    }
    for (path, bytes) in extra {
        entries.push(((*path).to_owned(), bytes.clone()));
    }
    Ok(common::repack(entries)?)
}

// ===========================================================================
// FINDINGS — role paths unvalidated: role member and its entries coverage may
// name different files, so open accepts an artifact whose consumed section is
// an uncovered decoy.
// ===========================================================================

#[test]
fn definition_path_decoy_forges_artifact_identity() -> Fallible {
    // The genuine `liasse.json` stays present and entries-covered. The manifest
    // definition role instead points at a decoy definition and declares the
    // decoy's D.4 identity. §19.5 fixes `definition.path` = "liasse.json"; open
    // MUST reject a role path that is not the canonical, entries-covered leaf.
    let decoy = br#"{"$app":"evil.forged@9.9.9","$liasse":1}"#.to_vec();
    let mut manifest = common::leaf_builder().manifest();
    let genuine_identity = manifest.definition.identity;
    manifest.definition.path = "liasse-decoy.json".to_owned();
    manifest.definition.identity = DefinitionId::of_canonical_bytes(&decoy);

    let bytes = repack_with(&manifest, &[("liasse-decoy.json", decoy.clone())])?;

    match Artifact::open(&bytes) {
        Err(_) => Ok(()), // spec-correct once the role path is constrained
        Ok(artifact) => {
            // Bug realized: the artifact's IDENTITY is over the decoy, not the
            // entries-certified genuine liasse.json.
            let over_decoy = artifact.definition_id() == DefinitionId::of_canonical_bytes(&decoy);
            let genuine_still_covered =
                artifact.entry(LIASSE_JSON_PATH) == Some(common::definition().as_slice());
            let consumed_is_decoy = artifact.liasse_json() == decoy.as_slice();
            Err(format!(
                "SECURITY (§19.5/Annex D.4): open ACCEPTED a manifest whose \
                 `definition.path` names decoy `liasse-decoy.json` instead of the \
                 fixed `liasse.json`. identity-over-decoy={over_decoy}, \
                 genuine-liasse.json-still-entries-covered={genuine_still_covered}, \
                 liasse_json()-returns-decoy={consumed_is_decoy}. The artifact \
                 identity ({:?}) is thus forged over an uncertified definition while \
                 the genuine definition ({:?}) is the one entries certifies.",
                artifact.definition_id(),
                genuine_identity,
            )
            .into())
        }
    }
}

#[test]
fn state_path_decoy_substitutes_consumed_state() -> Fallible {
    // The genuine `state/current.cbor.zst` stays present and entries-covered.
    // The state role points at an uncovered decoy leaf carrying the decoy's
    // checksum. §19.5 fixes `state.path` = "state/current.cbor.zst" and requires
    // entries to cover that leaf with an equal checksum; open must reject.
    let decoy = b"DECOY-STATE-INJECTED".to_vec();
    let mut manifest = common::leaf_builder().manifest();
    manifest.state.path = "state/decoy.cbor.zst".to_owned();
    manifest.state.sha256 = Digest::of_bytes(&decoy);

    let bytes = repack_with(&manifest, &[("state/decoy.cbor.zst", decoy.clone())])?;

    match Artifact::open(&bytes) {
        Err(_) => Ok(()),
        Ok(artifact) => {
            let consumed_is_decoy = artifact.state_section() == decoy.as_slice();
            let genuine_still_covered =
                artifact.entry(STATE_PATH) == Some(b"OPAQUE-STATE-SECTION".as_slice());
            Err(format!(
                "SECURITY (§19.5): open ACCEPTED a manifest whose `state.path` names \
                 uncovered decoy `state/decoy.cbor.zst`. state_section()-returns-decoy=\
                 {consumed_is_decoy}, genuine-state-still-entries-covered=\
                 {genuine_still_covered}. The runtime consumes a state the `entries` \
                 block never certifies — the §19.5 role-vs-coverage equality is vacuous."
            )
            .into())
        }
    }
}

#[test]
fn history_path_decoy_substitutes_consumed_history() -> Fallible {
    // Same root cause on the history role member.
    let decoy = br#"{"format":1,"selected":{"lineage":"evil","point":"pX"}}"#.to_vec();
    let mut manifest = common::leaf_builder().manifest();
    manifest.history.path = "history/decoy.json".to_owned();
    manifest.history.sha256 = Digest::of_bytes(&decoy);

    let bytes = repack_with(&manifest, &[("history/decoy.json", decoy.clone())])?;

    match Artifact::open(&bytes) {
        Err(_) => Ok(()),
        Ok(artifact) => {
            let consumed_is_decoy = artifact.history_index() == decoy.as_slice();
            Err(format!(
                "SECURITY (§19.5): open ACCEPTED a manifest whose `history.path` names \
                 uncovered decoy `history/decoy.json`; history_index()-returns-decoy=\
                 {consumed_is_decoy}. The consumed history index is not the \
                 entries-certified `history/index.json`."
            )
            .into())
        }
    }
}

// ===========================================================================
// PASSING CONTROLS — isolate the finding to the *unvalidated path*, proving the
// checksum machinery itself is sound when the role path is the canonical one.
// ===========================================================================

#[test]
fn control_genuine_leaf_opens() -> Fallible {
    Artifact::open(&common::leaf_bytes()?)?;
    Ok(())
}

#[test]
fn control_canonical_state_role_checksum_disagreement_rejected() -> Fallible {
    // With the CANONICAL state path, the role-member checksum and the entries
    // coverage checksum are verified against the SAME file, so a disagreement is
    // caught (transitive §19.5 "the two MUST be equal"). This is the property the
    // impl relies on; it holds ONLY because the path is canonical. Poison the
    // role-member checksum while leaving the entries coverage genuine.
    let mut manifest = common::leaf_builder().manifest();
    manifest.state.sha256 = Digest::of_bytes(b"a-checksum-that-matches-no-file");
    let bytes = repack_with(&manifest, &[])?;
    match Artifact::open(&bytes) {
        Err(ArtifactError::ChecksumMismatch { name, .. }) => {
            assert_eq!(name, STATE_PATH);
            Ok(())
        }
        other => {
            Err(format!("expected checksum mismatch on canonical state path, got {other:?}").into())
        }
    }
}

#[test]
fn control_canonical_entries_coverage_disagreement_rejected() -> Fallible {
    // The mirror direction: poison the entries coverage checksum for the
    // canonical state leaf while leaving the role member genuine. Still rejected,
    // proving the equality is enforced BOTH ways for the canonical path.
    let mut manifest = common::leaf_builder().manifest();
    if let Some(cov) = manifest.entries.get_mut(STATE_PATH) {
        cov.sha256 = Digest::of_bytes(b"a-checksum-that-matches-no-file");
    }
    let bytes = repack_with(&manifest, &[])?;
    match Artifact::open(&bytes) {
        Err(ArtifactError::ChecksumMismatch { name, .. }) => {
            assert_eq!(name, STATE_PATH);
            Ok(())
        }
        other => Err(format!(
            "expected checksum mismatch on canonical entries coverage, got {other:?}"
        )
        .into()),
    }
}

#[test]
fn control_builder_emits_the_fixed_role_literals() -> Fallible {
    // Sanity: the genuine builder emits the fixed literals the findings deviate
    // from, so the deviation — not some fixture quirk — is what open fails to
    // reject.
    let manifest = common::leaf_builder().manifest();
    assert_eq!(manifest.definition.path, LIASSE_JSON_PATH);
    assert_eq!(manifest.state.path, STATE_PATH);
    assert_eq!(manifest.history.path, HISTORY_INDEX_PATH);
    Ok(())
}
