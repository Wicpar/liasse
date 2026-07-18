//! Deletion policy (SPEC.md §21): `$on_delete` shapes and the deferred
//! delete-capability decision.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// §21.1 — a ref MAY omit `$on_delete` while nothing can delete its target.
#[test]
fn deferred_ref_without_capability_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.d@1.0.0", "$model": {
            "projects": { "$key": "id", "id": "text", "name": "text" }
            "tasks": { "$key": "id", "id": "text", "project": { "$ref": "/projects" } }
            "$mut": { "add_task": "return .tasks + { id: @id, project: @project }" }
        } }"#,
    );
    built.expect_ok();
}

/// §21.1 — introducing `collection - key` on the target activates the decision,
/// so an inbound ref that still omits `$on_delete` rejects the whole package.
#[test]
fn deleting_capability_forces_inbound_policy() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.d@1.0.0", "$model": {
            "projects": { "$key": "id", "id": "text" }
            "tasks": { "$key": "id", "id": "text", "project": { "$ref": "/projects" } }
            "$mut": { "delete_project": ".projects - @id" }
        } }"#,
    );
    assert!(built.has_code("M-DELETE"));
    assert!(built.has_hint());
}

/// §21.1 — declaring the policy lets the same package load.
#[test]
fn deciding_policy_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.d@1.0.0", "$model": {
            "projects": { "$key": "id", "id": "text" }
            "tasks": { "$key": "id", "id": "text", "project": { "$ref": "/projects", "$on_delete": "cascade" } }
            "$mut": { "delete_project": ".projects - @id" }
        } }"#,
    );
    built.expect_ok();
}

/// §21.1 / §5.6 — `none` is valid only for an optional ref.
#[test]
fn none_on_required_ref_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.d@1.0.0", "$model": {
            "projects": { "$key": "id", "id": "text" }
            "tasks": { "$key": "id", "id": "text", "project": { "$ref": "/projects", "$on_delete": "none" } }
            "$mut": { "delete_project": ".projects - @id" }
        } }"#,
    );
    assert!(built.has_code("M-DELETE"));
    assert!(built.has_hint());
}

/// §5.6 — `none` on an optional ref is accepted.
#[test]
fn none_on_optional_ref_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.d@1.0.0", "$model": {
            "projects": { "$key": "id", "id": "text" }
            "tasks": { "$key": "id", "id": "text", "project": { "$ref": "/projects", "$optional": true, "$on_delete": "none" } }
            "$mut": { "delete_project": ".projects - @id" }
        } }"#,
    );
    built.expect_ok();
}

/// §21.1 / §5.5 / §5.6 — a `$set` of `$ref` member is a governed inbound ref, so
/// leaving its `$on_delete` undecided while the target is deletable rejects the
/// whole package exactly as a scalar ref does (the set case must not slip past the
/// deferred-decision gate).
#[test]
fn set_of_ref_undecided_forces_inbound_policy() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.d@1.0.0", "$model": {
            "accounts": { "$key": "id", "id": "text" }
            "reviews": { "$key": "id", "id": "text", "reviewers": { "$set": { "$ref": "/accounts" } } }
            "$mut": { "delete_account": ".accounts - @id" }
        } }"#,
    );
    assert!(built.has_code("M-DELETE"));
    assert!(built.has_hint());
}

/// §21.1 / §5.6 — declaring the policy on the set element lets the same package
/// load (a decided set-of-ref member passes the gate).
#[test]
fn set_of_ref_deciding_policy_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.d@1.0.0", "$model": {
            "accounts": { "$key": "id", "id": "text" }
            "reviews": { "$key": "id", "id": "text", "reviewers": { "$set": { "$ref": "/accounts", "$on_delete": "cascade" } } }
            "$mut": { "delete_account": ".accounts - @id" }
        } }"#,
    );
    built.expect_ok();
}

/// §5.6 — `none` clears an *optional* ref, but a `$set` of `$ref` member is not an
/// optional field, so a decided `none` on a set element is rejected (there is no
/// single referencing field to assign `none` to).
#[test]
fn set_of_ref_none_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.d@1.0.0", "$model": {
            "accounts": { "$key": "id", "id": "text" }
            "reviews": { "$key": "id", "id": "text", "reviewers": { "$set": { "$ref": "/accounts", "$on_delete": "none" } } }
            "$mut": { "delete_account": ".accounts - @id" }
        } }"#,
    );
    assert!(built.has_code("M-DELETE"));
    assert!(built.has_hint());
}

/// §21.1 — an unknown policy word is rejected.
#[test]
fn unknown_policy_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.d@1.0.0", "$model": {
            "projects": { "$key": "id", "id": "text" }
            "tasks": { "$key": "id", "id": "text", "project": { "$ref": "/projects", "$on_delete": "drop" } }
        } }"#,
    );
    assert!(built.has_code("M-DELETE"));
}
