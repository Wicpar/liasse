//! Buckets (SPEC.md §14): lifecycle and source-backed period collections.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// §14.1 — a simple lifecycle bucket (short until-form) loads.
#[test]
fn lifecycle_bucket_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.b@1.0.0", "$model": {
            "sessions": {
              "$key": "id", "$bucket": ".expires_at",
              "id": "uuid = uuid()", "expires_at": "timestamp"
            }
        } }"#,
    );
    built.expect_ok();
}

/// §14.2 — the explicit `$from`/`$until` object form loads.
#[test]
fn explicit_lifecycle_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.b@1.0.0", "$model": {
            "reservations": {
              "$key": "id",
              "$bucket": { "$from": ".starts_at", "$until": ".ends_at" },
              "id": "uuid = uuid()", "starts_at": "timestamp", "ends_at": "timestamp"
            }
        } }"#,
    );
    built.expect_ok();
}

/// §14.4/§14.5 — a source-backed recurring bucket (the §15.3 `credit_periods`
/// shape) loads: `$source`/`$from`/`$until`/`$repeat` type in the source scope.
#[test]
fn source_backed_recurring_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.b@1.0.0", "$model": {
            "plans": { "$key": "id", "id": "text", "credits": "decimal", "period": "period?" }
            "subscriptions": {
              "$key": "id", "id": "uuid = uuid()", "plan": { "$ref": "/plans" },
              "starts_at": "timestamp", "ends_at": "timestamp? = none"
            }
            "credit_periods": {
              "$bucket": {
                "$source": ".subscriptions",
                "$from": "$source.starts_at",
                "$until": "$source.ends_at",
                "$repeat": "/plans[$source.plan].period"
              }
              "credits": "= /plans[$source.plan].credits"
            }
        } }"#,
    );
    built.expect_ok();
}

/// §14.2 — `$from` must be a timestamp; a text bound is rejected.
#[test]
fn non_timestamp_from_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.b@1.0.0", "$model": {
            "windows": {
              "$key": "id",
              "$bucket": { "$from": ".label" },
              "id": "text", "label": "text"
            }
        } }"#,
    );
    assert!(built.has_code("M-BUCKET"));
    assert!(built.has_hint());
}

/// §C.13 — an unknown `$bucket` object member is rejected.
#[test]
fn unknown_bucket_member_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.b@1.0.0", "$model": {
            "sessions": {
              "$key": "id",
              "$bucket": { "$until": ".expires_at", "$grace": ".x" },
              "id": "text", "expires_at": "timestamp"
            }
        } }"#,
    );
    assert!(built.has_code("M-BUCKET"));
    assert!(built.points_at("$grace"));
}

/// §14.4 — a source-backed bucket collection's rows are read-only; a mutation
/// that inserts into it is rejected.
#[test]
fn source_backed_insert_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.b@1.0.0", "$model": {
            "subscriptions": { "$key": "id", "id": "text", "starts_at": "timestamp", "ends_at": "timestamp?" }
            "access_periods": {
              "$bucket": { "$source": ".subscriptions", "$from": "$source.starts_at", "$until": "$source.ends_at" }
              "plan": "= $source.starts_at"
            }
            "$mut": { "forge": ".access_periods + { plan: 'forged' }" }
        } }"#,
    );
    assert!(built.has_code("M-MUT"));
    assert!(built.has_hint());
}
