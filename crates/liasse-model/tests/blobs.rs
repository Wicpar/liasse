//! Blobs (SPEC.md §18): accepted-type members and `$blob_storage` placement.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// §18.2/§18.4 — an accepted blob type and a placement policy load.
#[test]
fn accepted_blob_type_and_placement_load() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.bl@1.0.0", "$model": {
            "stores": { "$key": "id", "id": "text", "connector": "text", "enabled": "bool = true" }
            "docs": {
              "$key": "id", "id": "text",
              "$blob_storage": { "$in": { "$any": ["/stores['new']", "/stores['old']"] }, "$serve": "/stores['new']" }
              "file": { "$type": "blob", "$max_bytes": "10485760", "$media": ["application/pdf", "image/png"] }
            }
        } }"#,
    );
    built.expect_ok();
}

/// §18.4 — a `$copies` placement branch loads.
#[test]
fn copies_placement_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.bl@1.0.0", "$model": {
            "docs": {
              "$key": "id", "id": "text",
              "$blob_storage": { "$in": { "$copies": 2, "$of": "/stores[:s | s.region == 'eu']" } }
              "file": { "$type": "blob", "$max_bytes": "100", "$media": ["text/plain"] }
            }
        } }"#,
    );
    built.expect_ok();
}

/// A.8 — a blob field is not key-eligible, so `$key` on it is rejected.
#[test]
fn blob_key_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.bl@1.0.0", "$model": {
            "docs": { "$key": "file", "file": { "$type": "blob", "$max_bytes": "100", "$media": ["text/plain"] } }
        } }"#,
    );
    assert!(built.has_code("M-KEY"));
}

/// §18.2 — `$max_bytes`/`$media` are only valid on a `blob` field.
#[test]
fn accepted_members_on_nonblob_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.bl@1.0.0", "$model": {
            "docs": { "$key": "id", "id": "text", "file": { "$type": "text", "$max_bytes": "100" } }
        } }"#,
    );
    assert!(built.has_code("M-BLOB"));
    assert!(built.has_hint());
}

/// §18.4 / C.17 — `$blob_storage` requires `$in`.
#[test]
fn placement_without_in_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.bl@1.0.0", "$model": {
            "docs": {
              "$key": "id", "id": "text",
              "$blob_storage": { "$serve": "/stores['a']" }
              "file": { "$type": "blob", "$max_bytes": "100", "$media": ["text/plain"] }
            }
        } }"#,
    );
    assert!(built.has_code("M-MISSING"));
}

/// §18.4 — a `$copies` placement needs both `$copies` and `$of`.
#[test]
fn copies_without_of_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.bl@1.0.0", "$model": {
            "docs": {
              "$key": "id", "id": "text",
              "$blob_storage": { "$in": { "$copies": 2 } }
              "file": { "$type": "blob", "$max_bytes": "100", "$media": ["text/plain"] }
            }
        } }"#,
    );
    assert!(built.has_code("M-MISSING"));
}
