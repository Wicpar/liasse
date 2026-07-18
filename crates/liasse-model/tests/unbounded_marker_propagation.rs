//! §14.5 unbounded-recurring marker propagation through the view combinators
//! (`|`/`&`/`-`) and the branch forms (`? :` / `??`).
//!
//! §14.5 (SPEC.md): an unbounded recurring collection MUST be read through a
//! bounded temporal selector; the checker rejects an expression requiring
//! enumeration of an infinite series. §7.4: `a | b`, `a & b`, and `a - b` each
//! REQUIRE enumerating both operands, so the combined view is unbounded if EITHER
//! operand is. `? :` / `??` cannot statically be proven to skip a branch, so the
//! result is unbounded if EITHER branch is. In every case a bounding temporal
//! selector over the WHOLE combined/branched view still clears the marker and the
//! read loads — proving the guard is about enumeration, not the operator.
//!
//! These lock the two sides of the fix: an unbounded operand/branch with no
//! bounding selector is rejected, and a genuinely bounded combination (two bounded
//! operands, or the whole view under an outer selector) is NOT over-rejected.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// The shared model: an UNBOUNDED source-backed recurring bucket `credit_periods`
/// (optional `ends_at`, possibly-non-none plan period), so `.credit_periods` read
/// whole enumerates an infinite series. `read` exposes three `timestamp` params so
/// each case can bound with `.$at`/`.$between` and form a `bool` ternary condition
/// (`@a == @b`).
fn package(view: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "redteam.markerprop@1.0.0",
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

// --- Combinators: two bounded operands are NOT over-rejected (§7.4/§14.5) -----

/// Both union operands are bounded by their own `.$at`, so neither enumerates the
/// infinite series; the union is finite and MUST load. Guards against the fix
/// over-rejecting a genuinely bounded combination.
#[test]
fn union_of_two_bounded_operands_loads() {
    build(&package(".credit_periods.$at(@a) | .credit_periods.$at(@b)")).expect_ok();
}

/// Same for intersection: two bounded operands enumerate only finite slices, so
/// the intersection loads.
#[test]
fn intersection_of_two_bounded_operands_loads() {
    build(&package(".credit_periods.$at(@a) & .credit_periods.$at(@b)")).expect_ok();
}

// --- View difference `a - b` (§7.4) carries the same marker -------------------

/// `a - b` must enumerate every row of `b` to build the removal set, so an
/// unbounded right operand forces an infinite-series read that §14.5 rejects — the
/// same masking bug the union/intersection cases cover, in the difference operator.
#[test]
fn difference_with_unbounded_right_is_rejected() {
    assert!(
        build(&package(".credit_periods.$at(@t) - .credit_periods"))
            .result
            .is_err(),
        "`a - b` enumerates the unbounded recurring bucket in its right operand to \
         build the removal set (§7.4); §14.5 must reject it"
    );
}

/// Two bounded operands of a difference enumerate only finite slices and load.
#[test]
fn difference_of_two_bounded_operands_loads() {
    build(&package(".credit_periods.$at(@a) - .credit_periods.$at(@b)")).expect_ok();
}

// --- Ternary `cond ? then : otherwise` (§7.4) ---------------------------------

/// `@a == @b ? [] : .credit_periods` — the `otherwise` branch is the bare unbounded
/// recurring bucket. A static checker cannot prove the empty branch is always
/// taken, so the result is conservatively unbounded and §14.5 must reject the
/// unbounded, unbounded-selector-free read.
#[test]
fn ternary_with_unbounded_branch_is_rejected() {
    assert!(
        build(&package("@a == @b ? [] : .credit_periods"))
            .result
            .is_err(),
        "a `? :` whose branch is an unbounded recurring bucket, with no bounding \
         selector, must be rejected (§14.5): the checker cannot prove the branch is \
         never taken"
    );
}

/// A bounding selector over the WHOLE ternary result clears the marker and loads.
#[test]
fn ternary_bounded_by_outer_selector_loads() {
    build(&package("(@a == @b ? [] : .credit_periods).$at(@t)")).expect_ok();
}

/// Both ternary branches are bounded (or the empty view), so nothing enumerates an
/// infinite series; the ternary loads. Guards against over-rejection.
#[test]
fn ternary_with_two_bounded_branches_loads() {
    build(&package(
        "@a == @b ? .credit_periods.$at(@a) : .credit_periods.$at(@b)",
    ))
    .expect_ok();
}

// --- Fallback `a ?? b` (§7.4) -------------------------------------------------

/// `[] ?? .credit_periods` — the fallback branch is the bare unbounded recurring
/// bucket. A static checker cannot prove the fallback is never delivered, so the
/// result is conservatively unbounded and §14.5 must reject it.
#[test]
fn fallback_with_unbounded_branch_is_rejected() {
    assert!(
        build(&package("[] ?? .credit_periods")).result.is_err(),
        "a `??` whose fallback is an unbounded recurring bucket, with no bounding \
         selector, must be rejected (§14.5)"
    );
}

/// A bounding selector over the WHOLE fallback result clears the marker and loads.
#[test]
fn fallback_bounded_by_outer_selector_loads() {
    build(&package("([] ?? .credit_periods).$at(@t)")).expect_ok();
}

/// A fallback between two bounded views enumerates only finite slices and loads.
#[test]
fn fallback_of_two_bounded_operands_loads() {
    build(&package(
        ".credit_periods.$at(@a) ?? .credit_periods.$at(@b)",
    ))
    .expect_ok();
}
