//! Authenticators (SPEC.md §11): `$auth` declaration shape and role selection.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// §11.3 — the spec's session/api_key authenticator declarations load.
#[test]
fn authenticator_declarations_load() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.a@1.0.0",
            "$requires": { "cose": "liasse.cose@1", "api_keys": "acme.api_keys@1" },
            "$model": {
              "accounts": { "$key": "id", "id": "uuid = uuid()" }
              "integrations": { "$key": "id", "id": "uuid = uuid()" }
              "sessions": { "$key": "id", "id": "uuid = uuid()", "account": { "$ref": "/accounts" }, "revoked": "bool = false" }
              "$auth": {
                "session": {
                  "$credential": "bytes",
                  "$verify": "cose.verify(/session_keys, $credential)",
                  "$session": "/sessions[$proof.session]",
                  "$actor": "/accounts[$session.account]",
                  "$check": ["$proof.auth == $auth_name", "!$session.revoked"]
                }
                "api_key": {
                  "$credential": "text",
                  "$verify": "api_keys.verify($credential)",
                  "$actor": "/integrations[$proof.integration]",
                  "$check": "$proof.auth == $auth_name"
                }
              }
              "$roles": {
                "member": { "$auth": "session", "$members": ".accounts" }
                "automation": { "$auth": ["session", "api_key"], "$members": ".integrations" }
              }
            }
        }"#,
    );
    built.expect_ok();
}

/// C.12 — an authenticator requires `$actor`.
#[test]
fn authenticator_without_actor_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.a@1.0.0", "$model": {
            "$auth": { "session": { "$credential": "bytes", "$verify": "x.verify($credential)" } }
        } }"#,
    );
    assert!(built.has_code("M-MISSING"));
}

/// §2.5 / C.12 — an unknown authenticator member is rejected.
#[test]
fn unknown_authenticator_member_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.a@1.0.0", "$model": {
            "$auth": { "session": { "$credential": "bytes", "$verify": "x.v($credential)", "$actor": "/a[1]", "$scope": "y" } }
        } }"#,
    );
    assert!(built.has_code("M-AUTH"));
    assert!(built.points_at("$scope"));
}

/// §11.3 — a malformed `$credential` type is rejected.
#[test]
fn bad_credential_type_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.a@1.0.0", "$model": {
            "$auth": { "session": { "$credential": "notatype", "$verify": "x.v($credential)", "$actor": "/a[1]" } }
        } }"#,
    );
    assert!(built.has_code("M-AUTH"));
}

/// §11.4 — a role selecting an authenticator that no `$auth` block declares is
/// rejected.
#[test]
fn role_selects_undeclared_authenticator_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.a@1.0.0", "$model": {
            "accounts": { "$key": "id", "id": "uuid = uuid()" }
            "$roles": { "member": { "$auth": "ghost", "$members": ".accounts" } }
        } }"#,
    );
    assert!(built.has_code("M-AUTH"));
    assert!(built.has_hint());
}
