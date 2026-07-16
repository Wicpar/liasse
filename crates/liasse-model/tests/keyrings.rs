//! Keyrings (SPEC.md §17, C.16): the policy declaration's static shape.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// §17.1 — the spec's simple-form keyring policy loads (provider capability
/// resolution is a runtime seam, so no host is needed to validate the shape).
#[test]
fn simple_keyring_loads() {
    let built = build(
        r#"{
          "$liasse": 1, "$app": "t.k@1.0.0",
          "$model": {
            "session_keys": {
              "$keyring": {
                "$provider": "session-hsm", "$algorithm": "Ed25519",
                "$rotate": "P30D", "$retain": "P45D"
              }
            }
          }
        }"#,
    );
    built.expect_ok();
}

/// §17.1 — the controlled-policy form (object `$rotate`, `$usage`, `$protection`).
#[test]
fn controlled_keyring_loads() {
    let built = build(
        r#"{
          "$liasse": 1, "$app": "t.k@1.0.0",
          "$model": {
            "session_keys": {
              "$keyring": {
                "$provider": "session-hsm", "$algorithm": "ES256",
                "$usage": ["sign"],
                "$rotate": { "$every": "P30D", "$overlap": "P2D", "$mode": "automatic" },
                "$retain": "P45D", "$protection": "hardware"
              }
            }
          }
        }"#,
    );
    built.expect_ok();
}

/// §17.1 / C.16 — `$algorithm` is required; its omission is rejected.
#[test]
fn missing_algorithm_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.k@1.0.0", "$model": {
            "session_keys": { "$keyring": { "$provider": "hsm", "$rotate": "P30D" } }
        } }"#,
    );
    assert!(built.has_code("M-MISSING"));
    assert!(built.has_hint());
}

/// §2.5 / C.16 — a `$keyring` accepts no application-defined members.
#[test]
fn unknown_member_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.k@1.0.0", "$model": {
            "session_keys": { "$keyring": { "$provider": "hsm", "$algorithm": "Ed25519", "$expire": "P30D" } }
        } }"#,
    );
    assert!(built.has_code("M-KEYRING"));
    assert!(built.points_at("$expire"));
}

/// §17.1 — a malformed rotation cadence is caught at load.
#[test]
fn malformed_rotate_duration_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.k@1.0.0", "$model": {
            "session_keys": { "$keyring": { "$provider": "hsm", "$algorithm": "Ed25519", "$rotate": "every-month" } }
        } }"#,
    );
    assert!(built.has_code("M-KEYRING"));
}

/// C.16 — an object `$rotate` requires `$every`.
#[test]
fn rotate_object_requires_every_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.k@1.0.0", "$model": {
            "session_keys": { "$keyring": { "$provider": "hsm", "$algorithm": "Ed25519", "$rotate": { "$overlap": "P2D" } } }
        } }"#,
    );
    assert!(built.has_code("M-MISSING"));
}
