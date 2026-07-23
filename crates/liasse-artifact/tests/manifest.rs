//! Closed format-1 `manifest.json` parsing (SPEC.md §19.5, Annex D.5).
//!
//! Manifests here are crafted JSON bytes, so the expected outcome is
//! externally deducible from the §19.5 structure, not from this crate's own
//! serializer.

use liasse_artifact::{ArtifactError, Manifest};

type Fallible = Result<(), Box<dyn std::error::Error>>;

/// A minimal well-formed format-1 manifest (leaf artifact, no children).
const VALID: &str = r#"{
  "format": 1,
  "instance": "inst-root",
  "selected": { "lineage": "lin-1", "point": "p1" },
  "definition": { "identity": "sha256:0000000000000000000000000000000000000000000000000000000000000000", "path": "liasse.json" },
  "state": { "path": "state/current.cbor.zst", "sha256": "sha256:1111111111111111111111111111111111111111111111111111111111111111" },
  "history": { "path": "history/index.json", "sha256": "sha256:2222222222222222222222222222222222222222222222222222222222222222" },
  "coverage": { "included": { "lin-1": { "base": "p1", "tip": "p1" } }, "fully_restorable": true },
  "modules": {},
  "included_modules": {},
  "entries": {
    "mimetype": { "media": "text/plain", "sha256": "sha256:3333333333333333333333333333333333333333333333333333333333333333" }
  }
}"#;

#[test]
fn valid_manifest_parses() -> Fallible {
    let manifest = Manifest::parse(VALID.as_bytes())?;
    assert_eq!(manifest.instance.as_str(), "inst-root");
    assert_eq!(manifest.definition.path, "liasse.json");
    assert_eq!(manifest.selected.point().as_str(), "p1");
    assert!(manifest.entries.contains_key("mimetype"));
    Ok(())
}

#[test]
fn unknown_top_level_member_is_rejected() -> Fallible {
    // §19.5: "Additional members are invalid for format version 1."
    let text = VALID.replace("\"modules\": {},", "\"modules\": {}, \"vendor_extra\": 1,");
    match Manifest::parse(text.as_bytes()) {
        Err(ArtifactError::ManifestUnknownMember { name }) => assert_eq!(name, "vendor_extra"),
        other => return Err(format!("expected unknown-member error, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn missing_required_member_is_rejected() -> Fallible {
    let text = VALID.replace("\"instance\": \"inst-root\",", "");
    match Manifest::parse(text.as_bytes()) {
        Err(ArtifactError::ManifestMissingMember { member }) => assert_eq!(member, "instance"),
        other => return Err(format!("expected missing-member error, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn unsupported_format_is_rejected() -> Fallible {
    let text = VALID.replace("\"format\": 1,", "\"format\": 2,");
    match Manifest::parse(text.as_bytes()) {
        Err(ArtifactError::ManifestFormatUnsupported { found }) => assert_eq!(found, 2),
        other => return Err(format!("expected unsupported-format error, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn malformed_digest_is_rejected() -> Fallible {
    let text = VALID.replace(
        "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "not-a-digest",
    );
    match Manifest::parse(text.as_bytes()) {
        Err(ArtifactError::Digest(_)) => {}
        other => return Err(format!("expected digest error, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn non_json_is_rejected() -> Fallible {
    match Manifest::parse(b"not json at all") {
        Err(ArtifactError::ManifestJson { .. }) => {}
        other => return Err(format!("expected JSON error, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn canonical_bytes_are_unicode_sorted() -> Fallible {
    // D.4: canonical member order is Unicode scalar order. The top-level keys
    // must appear sorted, independent of serde_json's preserve_order feature.
    let manifest = Manifest::parse(VALID.as_bytes())?;
    let text = String::from_utf8(manifest.to_canonical_bytes())?;
    let order = [
        "\"coverage\"",
        "\"definition\"",
        "\"entries\"",
        "\"format\"",
        "\"history\"",
        "\"included_modules\"",
        "\"instance\"",
        "\"modules\"",
        "\"selected\"",
        "\"state\"",
    ];
    let mut last = 0usize;
    for key in order {
        let at = text.find(key).ok_or_else(|| format!("missing key {key}"))?;
        assert!(at >= last, "key {key} out of canonical order");
        last = at;
    }
    Ok(())
}
