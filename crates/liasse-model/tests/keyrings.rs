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

/// §17.2 — the ring's managed versions are exposed as a view, so a keyring
/// public selector (`.$current`, single active version §17.3; `.$versions`, a
/// version stream) type-checks against it in a `$view` position. Before the ring
/// was projected as a view it typed as opaque `json`, and the expression layer
/// rejected the selector as "applies to a view, not a json"; this locks in that
/// the selector resolves and the package loads.
#[test]
fn keyring_public_selectors_load_as_view() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.k@1.0.0", "$model": {
            "session_keys": {
              "$keyring": { "$provider": "hsm", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" }
            },
            "ring_current": { "$view": "/session_keys.$current" },
            "ring_versions": { "$view": "/session_keys.$versions" }
        } }"#,
    );
    built.expect_ok();
}

/// §17.2 — an unknown `.$name` structural selector on the ring is still rejected
/// (the ring being a view must not turn every `$`-suffixed access into a valid
/// selector), so the view fix does not mask a real error.
#[test]
fn keyring_unknown_selector_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.k@1.0.0", "$model": {
            "session_keys": {
              "$keyring": { "$provider": "hsm", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" }
            },
            "ring_bogus": { "$view": "/session_keys.$latest" }
        } }"#,
    );
    assert!(built.result.is_err());
}

/// §9.1 — a keyring-managed collection mirrors non-writable state (its versions
/// derive only from provider transitions, §17.2), so naming it in `$data`
/// is rejected. Seeding it would smuggle an attacker-controlled verification key
/// past the provider. The ring is modelled as a view (build/shapes.rs), so the
/// seed phase refuses it as non-writable derived state.
#[test]
fn keyring_managed_collection_not_seedable() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.k@1.0.0",
            "$model": {
              "session_keys": {
                "$keyring": { "$provider": "test-kp", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" }
              }
            },
            "$data": {
              "session_keys": {
                "$versions": [ { "id": "attacker-v1", "algorithm": "Ed25519", "public_key": "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a" } ]
              }
            }
        }"#,
    );
    assert!(built.has_code("M-SEED"), "expected a seed rejection, got: {}", built.rendered());
    assert!(built.rendered().contains("§9.1"));
}
