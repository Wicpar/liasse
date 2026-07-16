//! D.4/D.5/D.7 integrity: SHA-256 checked against independent standard
//! known-answer vectors (not against this crate's own output).

use liasse_ident::{DefinitionId, Digest, IdentError};

type Fallible = Result<(), Box<dyn std::error::Error>>;

// Standard NIST SHA-256 test vectors, computed independently of this crate.
const EMPTY: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const ABC: &str = "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

#[test]
fn digest_matches_known_answer_vectors() {
    assert_eq!(Digest::of_bytes(b"").to_canonical_text(), EMPTY);
    assert_eq!(Digest::of_bytes(b"abc").to_canonical_text(), ABC);
}

#[test]
fn definition_identity_matches_known_answer_vector() {
    // D.4: the definition identifier is SHA-256 over the canonical bytes, so a
    // known input yields the known standard digest.
    assert_eq!(
        DefinitionId::of_canonical_bytes(b"abc").to_canonical_text(),
        ABC
    );
}

#[test]
fn same_canonical_bytes_yield_same_definition_identity() {
    // D.4: the identifier covers only the inert definition bytes; identical
    // bytes (e.g. two exports differing only in selected state) share it.
    let a = DefinitionId::of_canonical_bytes(b"{\"$app\":\"t@1.0.0\"}");
    let b = DefinitionId::of_canonical_bytes(b"{\"$app\":\"t@1.0.0\"}");
    let other = DefinitionId::of_canonical_bytes(b"{\"$app\":\"t@1.0.1\"}");
    assert_eq!(a, b);
    assert_ne!(a, other);
}

#[test]
fn digest_text_round_trips() -> Fallible {
    let digest = Digest::of_bytes(b"abc");
    let parsed = Digest::parse(&digest.to_canonical_text())?;
    assert_eq!(parsed, digest);
    Ok(())
}

#[test]
fn digest_parse_accepts_uppercase_hex() -> Fallible {
    // Input hex case is unpinned (SPEC-ISSUES item 20 by analogy); either case
    // decodes to the same digest and renders lowercase.
    let upper = "sha256:BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD";
    let parsed = Digest::parse(upper)?;
    assert_eq!(parsed.to_canonical_text(), ABC);
    Ok(())
}

#[test]
fn digest_parse_rejects_malformed_text() {
    // Missing prefix.
    assert!(matches!(
        Digest::parse("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        Err(IdentError::MalformedDigest { .. })
    ));
    // Wrong byte length.
    assert!(matches!(
        Digest::parse("sha256:abcd"),
        Err(IdentError::MalformedDigest { .. })
    ));
    // Non-hex payload.
    assert!(matches!(
        Digest::parse("sha256:zz"),
        Err(IdentError::MalformedDigest { .. })
    ));
}
