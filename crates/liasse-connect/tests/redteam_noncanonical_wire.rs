#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Strict wire canonicality (SPEC-ISSUES item 2) at the connect hostile boundary.
//!
//! A client request whose scalar argument is spelled non-canonically is REJECTED
//! at the §12.1 wire-decode boundary as malformed — before any receiver-key
//! comparison, so the rejection is independent of a key collision. This pins the
//! stronger property the `uuid-uppercase-input-canonicalization` corpus case can
//! also reach via a duplicate-key collapse: a non-canonical scalar never even
//! decodes into a request argument, so it can neither mint a second identity nor
//! alias an existing row.
//!
//! Externally deducible: Annex A.1 / D.2 fix a `uuid`'s canonical wire/key text as
//! the lowercase hyphenated string and an `int`'s as its shortest decimal with no
//! leading zero or sign; item 2 pins that a machine wire/request scalar MUST be
//! canonical, a non-canonical spelling refused rather than normalized. The
//! authoring boundary (`Type::decode`) stays lenient and canonicalizes the SAME
//! spellings — asserted here too, so the split is pinned rather than the codec's
//! own answer being read back at itself.

use std::collections::BTreeMap;

use liasse_connect::DecodeError;
use liasse_connect::decode::decode_args;
use liasse_value::{Type, Value};
use liasse_wire::serde_json::json;

/// A two-argument request contract: a `uuid` identity and an `int` count — the two
/// scalar types whose canonical wire text A.1 / D.2 pin most sharply.
fn contract() -> Vec<(String, Type)> {
    vec![("id".to_owned(), Type::Uuid), ("count".to_owned(), Type::Int)]
}

#[test]
fn canonical_scalar_args_decode_at_the_request_boundary() {
    let wire = json!({
        "id": "0191c1a0-0000-7000-8000-000000000000",
        "count": "20",
    });
    let decoded: BTreeMap<String, Value> = decode_args(&contract(), Some(&wire))
        .expect("canonical wire scalars decode at the request boundary");
    assert!(matches!(decoded.get("id"), Some(Value::Uuid(_))));
    assert!(matches!(decoded.get("count"), Some(Value::Int(_))));
}

#[test]
fn uppercase_uuid_arg_is_rejected_at_the_wire_boundary() {
    // The same UUID value the canonical case accepts, with uppercased hex digits: a
    // non-canonical wire spelling. It must be refused as malformed at decode, never
    // normalized to the lowercase form (which would let it alias an existing row by
    // key). No receiver key is involved here, so this is a pure wire-boundary
    // rejection, not a key collision.
    let wire = json!({ "id": "0191C1A0-0000-7000-8000-000000000000" });
    let error = decode_args(&contract(), Some(&wire))
        .expect_err("a non-canonical uuid argument is rejected at the wire boundary");
    assert!(matches!(error, DecodeError::Malformed(_)), "got {error:?}");
}

#[test]
fn leading_zero_int_arg_is_rejected_at_the_wire_boundary() {
    // `"020"` denotes the same integer as the canonical `"20"`; the non-canonical
    // leading zero is refused rather than stripped.
    let wire = json!({ "count": "020" });
    let error = decode_args(&contract(), Some(&wire))
        .expect_err("a leading-zero int argument is rejected at the wire boundary");
    assert!(matches!(error, DecodeError::Malformed(_)), "got {error:?}");
}

#[test]
fn authoring_boundary_canonicalizes_the_same_spellings() {
    // The split the resolution turns on: the SAME non-canonical uuid the wire
    // boundary refuses is accepted and canonicalized at the authoring boundary
    // (`Type::decode`), collapsing onto the one lowercase identity (item 2 scope).
    let upper = Type::Uuid
        .decode(&json!("0191C1A0-0000-7000-8000-000000000000"))
        .expect("authoring decode canonicalizes an uppercase uuid");
    let lower = Type::Uuid
        .decode(&json!("0191c1a0-0000-7000-8000-000000000000"))
        .expect("authoring decode of the canonical uuid");
    assert_eq!(upper, lower, "authoring canonicalizes to the one identity");
}
