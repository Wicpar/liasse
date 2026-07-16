#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! The §16/§17.8 COSE token codec and descriptor: the claims carry and bind the
//! authenticator identity, the token value round-trips through its pinned wire
//! form, and the `liasse.cose@1` descriptor pins `sign` as generated and
//! `verify` as a verifier (§16.3).

use liasse_host::{cose_descriptor, CoseClaims, CoseToken, EffectClass};
use liasse_value::{Text, Value};

fn claims() -> CoseClaims {
    CoseClaims::new([
        (Text::new("auth"), Value::Text(Text::new("session"))),
        (Text::new("session"), Value::Text(Text::new("s-1"))),
    ])
}

/// §11.4: the `auth` claim binds a token to its issuing authenticator, and is
/// recoverable from the claims.
#[test]
fn claims_bind_the_authenticator() {
    let claims = claims();
    assert_eq!(claims.auth(), Some("session"));
    assert_eq!(claims.get("session"), Some(&Value::Text(Text::new("s-1"))));
}

/// The canonical signing bytes are stable across equal claim sets — a verifier
/// re-derives exactly the bytes the signer signed (§17.8).
#[test]
fn signing_bytes_are_deterministic() {
    let a = CoseClaims::new([
        (Text::new("auth"), Value::Text(Text::new("session"))),
        (Text::new("session"), Value::Text(Text::new("s-1"))),
    ]);
    let b = CoseClaims::new([
        // Insertion order reversed; the canonical form is order-independent.
        (Text::new("session"), Value::Text(Text::new("s-1"))),
        (Text::new("auth"), Value::Text(Text::new("session"))),
    ]);
    assert_eq!(a.signing_bytes(), b.signing_bytes());
    // A different claim value changes the signed bytes.
    let c = CoseClaims::new([(Text::new("auth"), Value::Text(Text::new("other")))]);
    assert_ne!(a.signing_bytes(), c.signing_bytes());
}

/// §17.8 pinned format: a token round-trips through its typed value form,
/// preserving ring, version, claims, and signature.
#[test]
fn token_round_trips_through_its_value_form() {
    let token = CoseToken::new("session_keys", 2, claims(), b"sig-bytes".to_vec());
    let value = token.to_value();
    let recovered = CoseToken::from_value(&value).expect("a well-formed token round-trips");
    assert_eq!(recovered, token);
    assert_eq!(recovered.ring(), "session_keys");
    assert_eq!(recovered.version(), 2);
    assert_eq!(recovered.signature(), b"sig-bytes");
    assert_eq!(recovered.claims().auth(), Some("session"));
}

/// A value that is not a well-formed token is rejected rather than half-parsed.
#[test]
fn malformed_token_value_rejected() {
    assert_eq!(CoseToken::from_value(&Value::Text(Text::new("nope"))), None);
}

/// §16.2/§16.3: the pinned descriptor declares `sign` generated and `verify` a
/// verifier, and names the `token` value type.
#[test]
fn descriptor_pins_effect_classes() {
    let descriptor = cose_descriptor();
    assert_eq!(descriptor.id().as_str(), "liasse.cose");
    assert_eq!(descriptor.version().major, 1);
    assert_eq!(descriptor.function("sign").expect("sign").effect(), EffectClass::Generated);
    assert_eq!(descriptor.function("verify").expect("verify").effect(), EffectClass::Verifier);
    assert!(descriptor.named_type("token").is_some());
}
