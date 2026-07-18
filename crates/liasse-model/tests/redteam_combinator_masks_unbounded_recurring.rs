//! RED-TEAM: a view combinator masks the §14.5 unbounded-recurring enumeration
//! guard.
//!
//! §14.5 (SPEC.md): "An unbounded recurring collection MUST be read through a
//! bounded temporal selector such as `.$at` or `.$between`. The checker rejects an
//! expression requiring enumeration of an infinite series." §7.4: `a | b` is
//! "union, left order then new right identities" and `a & b` is "intersection" —
//! both REQUIRE enumerating every row of the right operand to compute the result.
//! So `.credit_periods.$at(@t) | .credit_periods` unconditionally enumerates
//! `.credit_periods` (the bare unbounded recurring bucket) whole, which §14.5
//! forbids — the bounded LEFT operand does not save it.
//!
//! Root cause: the §14.5 terminal guard fires only on the whole expression's type
//! (`check_expression`, crates/liasse-expr/src/check/mod.rs), and the combinator
//! checker builds the result row from the LEFT operand alone (`check_combination`,
//! crates/liasse-expr/src/check/ops.rs — `acc_row` is the left's row), dropping the
//! RIGHT operand's unbounded-recurring marker. A bounded left therefore yields a
//! not-unbounded result type, the guard never fires, and the package loads.
//!
//! Every expectation is deducible from SPEC.md text alone (§14.5 + §7.4). The bare
//! and selector-last controls anchor both sides: the bare read is rejected, the
//! selector-last read loads, so the combinator masking is the only variable.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// The shared `credit_periods` model: an UNBOUNDED source-backed recurring bucket
/// (optional `ends_at`, possibly-non-none plan period), reused by every case so the
/// only difference is the view expression under test.
fn package(view: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "redteam.combmask@1.0.0",
          "$model": {{
            "plans": {{ "$key": "id", "id": "text", "credits": "decimal", "period": "period?" }},
            "subscriptions": {{
              "$key": "id", "id": "text", "plan": {{ "$ref": "/plans" }},
              "starts_at": "timestamp", "ends_at": "timestamp?"
            }},
            "credit_periods": {{
              "$bucket": {{
                "$source": ".subscriptions",
                "$from": "$source.starts_at",
                "$until": "$source.ends_at",
                "$repeat": "/plans[$source.plan].period"
              }},
              "credits": "= /plans[$source.plan].credits"
            }},
            "$public": {{
              "read": {{ "$params": {{ "t": "timestamp", "a": "timestamp", "b": "timestamp" }},
                        "$view": "{view}" }}
            }}
          }}
        }}"#
    )
}

/// Positive control (§14.5): a bare read of the unbounded recurring bucket, with no
/// bounding temporal selector, MUST be rejected. Anchors that the guard is real and
/// reachable from a `$public` surface view.
#[test]
fn control_bare_unbounded_read_is_rejected() {
    let built = build(&package(".credit_periods { credits }"));
    assert!(
        built.result.is_err(),
        "a bare unbounded recurring read must be rejected (§14.5), but the package loaded"
    );
}

/// Positive control (§14.5): giving the bounding selector the LAST word over the
/// WHOLE combined view — `(.credit_periods | .credit_periods).$at(@t)` — is a bounded
/// read and MUST load. This is the accepted form the masking cases should have been
/// steered toward, and it proves the rejection below is about enumeration, not about
/// combinators per se.
#[test]
fn control_selector_over_whole_union_loads() {
    let built = build(&package("(.credit_periods | .credit_periods).$at(@t)"));
    built.expect_ok();
}

/// THE BUG (§14.5 + §7.4): `.credit_periods.$at(@t) | .credit_periods`. The union's
/// RIGHT operand is the bare unbounded recurring bucket. `|` is "union, left order
/// then new right identities" (§7.4): computing it REQUIRES enumerating every right
/// row, so the expression requires enumerating the infinite series whole. §14.5
/// mandates rejection. The bounded left operand does not bound the right.
///
/// Currently FAILS: `check_combination` builds the result row from the bounded LEFT
/// operand, so the terminal §14.5 guard sees a not-unbounded type and the package
/// loads.
#[test]
fn union_with_bounded_left_and_unbounded_right_is_rejected() {
    let built = build(&package(".credit_periods.$at(@t) | .credit_periods"));
    assert!(
        built.result.is_err(),
        "`.credit_periods.$at(@t) | .credit_periods` requires enumerating the unbounded \
         recurring bucket in its RIGHT operand to compute the union (§7.4), so §14.5 must \
         reject it; instead the package loaded — the bounded left operand masked the \
         right operand's unbounded-recurring marker (check_combination, \
         crates/liasse-expr/src/check/ops.rs)"
    );
}

/// Sibling of the union bug for the intersection combinator (§7.4 `a & b`). An
/// intersection likewise enumerates both operands, so the bare unbounded right
/// operand forces an infinite-series read that §14.5 must reject.
///
/// Currently FAILS for the same root cause.
#[test]
fn intersection_with_bounded_left_and_unbounded_right_is_rejected() {
    let built = build(&package(".credit_periods.$at(@t) & .credit_periods"));
    assert!(
        built.result.is_err(),
        "`.credit_periods.$at(@t) & .credit_periods` enumerates the unbounded recurring \
         bucket in its right operand to compute the intersection (§7.4); §14.5 must reject \
         it, but the package loaded"
    );
}
