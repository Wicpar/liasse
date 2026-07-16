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

/// §18.1 — a blob descriptor's members (`$sha512`, `$bytes`, `$media`, `$name`)
/// are readable in computed values. Loading proves all four type-check; the
/// `$bytes + 1` use additionally proves `$bytes` typed as a *number* (integer
/// `+` an `int` literal, not text concatenation), and `size(.file.$media)`
/// proves `$media` typed as `text`.
#[test]
fn descriptor_members_readable_in_computed_values() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.bl@1.0.0", "$model": {
            "docs": {
              "$key": "id", "id": "text",
              "file": { "$type": "blob", "$max_bytes": "100", "$media": ["text/plain"] },
              "next_size": "= .file.$bytes + 1",
              "hash": "= .file.$sha512",
              "media_len": "= size(.file.$media)",
              "label": "= .file.$name"
            }
        } }"#,
    );
    built.expect_ok();
}

/// §18.1 — `$bytes` is an `int`, not text: adding it to a `text` value is a
/// static type error, so the number typing is not vacuous.
#[test]
fn bytes_member_is_numeric_not_text() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.bl@1.0.0", "$model": {
            "docs": {
              "$key": "id", "id": "text",
              "file": { "$type": "blob", "$max_bytes": "100", "$media": ["text/plain"] },
              "bad": "= .file.$bytes + 'x'"
            }
        } }"#,
    );
    assert!(built.has_code("E-EXPR"));
}

/// §18.1 — a blob descriptor member selector applies only to a `blob`; reading
/// `.$bytes` off a non-blob field is a static type error.
#[test]
fn descriptor_member_on_non_blob_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.bl@1.0.0", "$model": {
            "docs": { "$key": "id", "id": "text", "count": "int", "bad": "= .count.$bytes" }
        } }"#,
    );
    assert!(built.has_code("E-EXPR"));
}
