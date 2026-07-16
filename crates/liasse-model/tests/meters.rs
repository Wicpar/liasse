//! Meters (SPEC.md §15): `$limits`/`$consumes` shape and meter reachability.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// §15.1 — the spec's simple-credits meter loads.
#[test]
fn simple_credits_meter_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.m@1.0.0", "$model": {
            "users": {
              "$key": "id", "id": "uuid = uuid()",
              "topups": {
                "$key": "id", "$bucket": { "$until": ".expires_at" },
                "id": "uuid = uuid()", "amount": "decimal", "expires_at": "timestamp? = none"
              }
              "spends": {
                "$key": "id", "$consumes": "credits",
                "id": "uuid = uuid()", "amount": "decimal", "occurred_at": "timestamp = now()"
              }
              "$limits": {
                "credits": {
                  "$sources": { "topup": ".topups { $quantity: .amount }" }
                  "$order": ["$until"]
                }
              }
            }
        } }"#,
    );
    built.expect_ok();
}

/// §15.4 — a meter declared on an ancestor is reachable from a descendant
/// spend (`$consumes` on `users.spends` resolving `users.$limits.credits`).
#[test]
fn ancestor_meter_is_reachable() {
    // Already exercised above; here the object `$consumes` form resolves too.
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.m@1.0.0", "$model": {
            "users": {
              "$key": "id", "id": "text",
              "spends": {
                "$key": "id", "$consumes": { "credits": ".amount" },
                "id": "text", "amount": "decimal"
              }
              "$limits": { "credits": { "$sources": { "topup": ".topups { $quantity: .amount }" } } }
              "topups": { "$key": "id", "id": "text", "amount": "decimal" }
            }
        } }"#,
    );
    built.expect_ok();
}

/// §15.1 — `$consumes` naming a meter no ancestor declares is rejected.
#[test]
fn consumes_unknown_meter_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.m@1.0.0", "$model": {
            "users": {
              "$key": "id", "id": "text",
              "spends": { "$key": "id", "$consumes": "ghost", "id": "text", "amount": "decimal" }
            }
        } }"#,
    );
    assert!(built.has_code("M-METER"));
    assert!(built.has_hint());
}

/// C.14 — a meter declaration requires `$sources`.
#[test]
fn meter_without_sources_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.m@1.0.0", "$model": {
            "users": {
              "$key": "id", "id": "text",
              "$limits": { "credits": { "$order": ["$until"] } }
            }
        } }"#,
    );
    assert!(built.has_code("M-MISSING"));
}

/// §15.3 / §15.6 — a spending collection's rows expose a `funding` accessor, so
/// a view projecting `.spends { …, funding }` types (the accessor no longer
/// rejects as an unknown name). The view is checked strictly against the spend
/// row type.
#[test]
fn spend_funding_accessor_types() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.m@1.0.0", "$model": {
            "users": {
              "$key": "id", "id": "text",
              "topups": { "$key": "id", "id": "text", "amount": "decimal" },
              "spends": {
                "$key": "id", "$consumes": "credits",
                "id": "uuid = uuid()", "amount": "decimal"
              }
              "$limits": {
                "credits": { "$sources": { "topup": ".topups { $quantity: .amount }" } }
              }
              "history": { "$view": ".spends { id, amount, funding }" }
            }
        } }"#,
    );
    built.expect_ok();
}

/// §15.6 — `funding` is only exposed on a spending collection; a collection with
/// no `$consumes` has no `funding` field, so a view referencing it is rejected.
/// This keeps the accessor gated on the meter relationship rather than universal.
#[test]
fn funding_absent_on_non_consuming_collection() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.m@1.0.0", "$model": {
            "users": {
              "$key": "id", "id": "text",
              "notes": { "$key": "id", "id": "text", "body": "text" }
              "recent": { "$view": ".notes { id, funding }" }
            }
        } }"#,
    );
    built.expect_err();
}

/// §2.5 / C.14 — an unknown meter member is rejected.
#[test]
fn unknown_meter_member_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.m@1.0.0", "$model": {
            "users": {
              "$key": "id", "id": "text",
              "$limits": { "credits": { "$sources": { "t": ".topups" }, "$cap": "10" } }
            }
        } }"#,
    );
    assert!(built.has_code("M-METER"));
    assert!(built.points_at("$cap"));
}
