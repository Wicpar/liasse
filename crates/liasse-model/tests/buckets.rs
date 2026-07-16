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

/// §14.4/§14.1 — a temporal selector over a source-backed bucket type-checks:
/// `.access_periods.$between(@a, @b)` yields a view whose projection may read the
/// derived output fields (`plan`) and the structural bindings
/// (`$index`/`$from`/`$until`). The package loads only if the source bucket is
/// typed as a temporal collection, not an opaque scalar.
#[test]
fn source_bucket_temporal_selector_projection_types() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.b@1.0.0", "$model": {
            "subscriptions": { "$key": "id", "id": "text", "plan": "text", "starts_at": "timestamp", "ends_at": "timestamp?" }
            "access_periods": {
              "$bucket": { "$source": ".subscriptions", "$from": "$source.starts_at", "$until": "$source.ends_at" }
              "plan": "= $source.plan"
            }
            "$public": {
              "periods": {
                "$params": { "a": "timestamp", "b": "timestamp" }
                "$view": ".access_periods.$between(@a, @b) { index: $index, from: $from, until: $until, plan }"
              }
            }
        } }"#,
    );
    built.expect_ok();
}

/// §14.5 — a bare projection over an unbounded recurring bucket (optional series
/// upper bound) is rejected: it must be read through a bounded temporal selector.
#[test]
fn unbounded_recurring_bare_enumeration_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.b@1.0.0", "$model": {
            "plans": { "$key": "id", "id": "text", "credits": "decimal", "period": "period?" }
            "subscriptions": {
              "$key": "id", "id": "text", "plan": { "$ref": "/plans" },
              "starts_at": "timestamp", "ends_at": "timestamp?"
            }
            "credit_periods": {
              "$bucket": {
                "$source": ".subscriptions", "$from": "$source.starts_at",
                "$until": "$source.ends_at", "$repeat": "/plans[$source.plan].period"
              }
              "credits": "= /plans[$source.plan].credits"
            }
            "all_periods": { "$view": ".credit_periods { credits }" }
        } }"#,
    );
    built.expect_err();
    assert!(built.codes().iter().any(|c| c == "E-EXPR" || c == "M-EXPR"));
}

/// §14.5 — the same unbounded recurring bucket read through a bounded selector
/// (`.$between`) type-checks: the bounded window lifts the enumeration guard.
#[test]
fn unbounded_recurring_bounded_selector_types() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.b@1.0.0", "$model": {
            "plans": { "$key": "id", "id": "text", "credits": "decimal", "period": "period?" }
            "subscriptions": {
              "$key": "id", "id": "text", "plan": { "$ref": "/plans" },
              "starts_at": "timestamp", "ends_at": "timestamp?"
            }
            "credit_periods": {
              "$bucket": {
                "$source": ".subscriptions", "$from": "$source.starts_at",
                "$until": "$source.ends_at", "$repeat": "/plans[$source.plan].period"
              }
              "credits": "= /plans[$source.plan].credits"
            }
            "$public": {
              "window": {
                "$params": { "a": "timestamp", "b": "timestamp" }
                "$view": ".credit_periods.$between(@a, @b) { index: $index, credits }"
              }
            }
        } }"#,
    );
    built.expect_ok();
}

/// §14.1/§8.3 — a mutation parameter used only as a temporal-selector instant is
/// inferred as `timestamp`, so the mutation's parameter contract carries it.
#[test]
fn temporal_selector_param_inferred_as_timestamp() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.b@1.0.0", "$model": {
            "sessions": {
              "$key": "id", "$bucket": ".expires_at",
              "id": "text", "expires_at": "timestamp"
            }
            "$mut": { "window": "return .sessions.$between(@a, @b) { id }" }
        } }"#,
    );
    let model = built.expect_ok();
    let mutation = model
        .mutations()
        .iter()
        .find(|m| m.name.as_str() == "window")
        .expect("the `window` mutation is validated");
    for name in ["a", "b"] {
        let (_, ty) = mutation
            .params
            .iter()
            .find(|(param, _)| param == name)
            .unwrap_or_else(|| panic!("parameter `@{name}` is inferred"));
        assert!(
            matches!(ty.as_scalar(), Some(liasse_value::Type::Timestamp(_))),
            "`@{name}` inferred as timestamp, got {ty:?}"
        );
    }
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
