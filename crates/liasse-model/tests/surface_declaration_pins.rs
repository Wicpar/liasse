//! §10.1 surface well-formedness pins (SPEC-ISSUES #10 and #11(c)).
//!
//! #10 — a surface `$view` parameter is NOT inferred: every `@name` a surface
//! `$view` reads MUST be declared in the surface's `$params`, and an undeclared
//! `@name` is a static load error (§8.3 inference is mutation-only). The public
//! `$view` path already reaches this rejection through full type-checking; the
//! ROLE `$view` path skips full typing for the `$actor` seam, so the model must
//! reject an undeclared `@name` there explicitly. Both paths must agree.
//!
//! #11(c) — a surface MUST declare at least one of `$view` or `$mut`. An empty
//! surface, or one carrying only `$params` and/or `$recursive`, exposes nothing
//! callable or watchable and is rejected at load.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// A model root carrying `$model_body` verbatim under `$model`, with the accounts
/// and tasks collections and a `token` authenticator every surface case reuses.
fn model(model_body: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "pins.surface@1.0.0",
          "$model": {{
            "accounts": {{ "$key": "id", "id": "text", "enabled": "bool = true" }},
            "tasks": {{ "$key": "id", "id": "text", "done": "bool" }},
            "$auth": {{
              "token": {{
                "$credential": "text",
                "$verify": "$credential",
                "$actor": "/accounts[$proof]"
              }}
            }},
            {model_body}
          }}
        }}"#
    )
}

/// A `$public` block exposing one surface `void` with `$body` members.
fn public_surface(body: &str) -> String {
    model(&format!(r#""$public": {{ "void": {{ {body} }} }}"#))
}

/// A `$roles` block whose `member` role grants one surface `by_state` with
/// `$body` members. `$roles` is at the model root, so the role surface's `.`
/// receiver is the model root (identical to a public surface's `.`), isolating
/// the surface body as the only variable.
fn role_surface(body: &str) -> String {
    model(&format!(
        r#""$roles": {{
          "member": {{
            "$auth": "token",
            "$members": ".accounts[:a | a.enabled]",
            "by_state": {{ {body} }}
          }}
        }}"#
    ))
}

// --- #10: surface `$view` parameter must be declared -----------------------

/// Control: a role surface whose `$view` reads `@done` AND declares it in
/// `$params` loads. This proves the role scaffold is well-formed, so the only
/// variable in the rejection case below is the missing `$params` declaration.
#[test]
fn role_view_declared_parameter_loads() {
    let built = build(&role_surface(
        r#""$params": { "done": "bool" }, "$view": ".tasks[:t | t.done == @done] { id }""#,
    ));
    built.expect_ok();
}

/// #10: a ROLE surface `$view` reading `@done` with no `$params` entry is a
/// static load error. This is the path the fix targets — a role `$view` skips
/// full typing (the `$actor` seam), so the undeclared-parameter rejection must be
/// enforced explicitly. Rejected under the surface code (M-SURFACE).
#[test]
fn role_view_undeclared_parameter_rejected() {
    let built = build(&role_surface(r#""$view": ".tasks[:t | t.done == @done] { id }""#));
    assert!(
        built.result.is_err(),
        "a role surface `$view` reading the undeclared `@done` must be rejected (§10.1); a \
         surface view parameter is not inferred (§8.3 is mutation-only)"
    );
    assert!(
        built.has_code("M-SURFACE"),
        "expected the surface diagnostic (M-SURFACE) on the role view, got: {:?}",
        built.codes()
    );
}

/// #10: the byte-identical PUBLIC surface `$view` is rejected too — both paths
/// agree that an undeclared surface-view parameter is a static load error. (The
/// public path reaches it through full type-checking, so its code need not be
/// M-SURFACE; only the reject decision must match.)
#[test]
fn public_view_undeclared_parameter_rejected() {
    let built = build(&public_surface(r#""$view": ".tasks[:t | t.done == @done] { id }""#));
    assert!(
        built.result.is_err(),
        "a public surface `$view` reading the undeclared `@done` must be rejected (§10.1)"
    );
}

/// Control: the same public surface with `@done` declared in `$params` loads, so
/// the rejection above is caused by the missing declaration, not the projection.
#[test]
fn public_view_declared_parameter_loads() {
    let built = build(&public_surface(
        r#""$params": { "done": "bool" }, "$view": ".tasks[:t | t.done == @done] { id }""#,
    ));
    built.expect_ok();
}

/// A self-referential `companies` collection whose scoped `admin` role grants a
/// `company` surface with a `$view` and a `$recursive` block whose `$where`
/// predicate body is `$where`. The role scope is the company row (§10.3), so the
/// only variable is the predicate text.
fn recursive_where(where_pred: &str, params: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "pins.recursive@1.0.0",
          "$model": {{
            "accounts": {{ "$key": "id", "id": "text" }},
            "companies": {{
              "$key": "id",
              "id": "text",
              "plan": "text = 'active'",
              "subcompanies": {{ "$like": "^" }},
              "members": {{
                "$key": "account",
                "account": {{ "$ref": "/accounts" }},
                "admin": "bool = false"
              }},
              "$roles": {{
                "admin": {{
                  "$auth": "token",
                  "$members": ".members[:m | m.admin].account",
                  "company": {{
                    {params}
                    "$view": ". {{ id, plan }}",
                    "$recursive": {{
                      "$field": "subcompanies",
                      "$through": ".subcompanies",
                      "$bind": "child",
                      "$where": "{where_pred}"
                    }}
                  }}
                }}
              }}
            }},
            "$auth": {{
              "token": {{
                "$credential": "text",
                "$verify": "$credential",
                "$actor": "/accounts[$proof]"
              }}
            }}
          }}
        }}"#
    )
}

/// #10 (the `$recursive` predicate sub-case): a `$where` predicate reading an
/// undeclared `@plan` is rejected. This path already types the predicate fully
/// (params in scope), so `liasse-expr` reports the unknown parameter — the SPEC
/// pin matches the existing behavior, no new code needed here.
#[test]
fn recursive_where_undeclared_parameter_rejected() {
    let built = build(&recursive_where("child.plan != @plan", ""));
    assert!(
        built.result.is_err(),
        "a `$recursive` `$where` reading the undeclared `@plan` must be rejected (§10.1/§10.5)"
    );
}

/// Control: the same `$where` with `@plan` declared in `$params` loads, proving
/// the rejection above is the missing declaration, not the predicate itself.
#[test]
fn recursive_where_declared_parameter_loads() {
    let built = build(&recursive_where("child.plan != @plan", r#""$params": { "plan": "text" },"#));
    built.expect_ok();
}

// --- #11(c): a surface must expose `$view` or `$mut` ------------------------

/// #11(c): an empty surface `{}` exposes nothing callable or watchable and is
/// rejected at load under the surface code.
#[test]
fn empty_surface_rejected() {
    let built = build(&public_surface(""));
    assert!(
        built.result.is_err(),
        "an empty surface exposes nothing and must be rejected at load (§10.1)"
    );
    assert!(
        built.has_code("M-SURFACE"),
        "expected the surface diagnostic (M-SURFACE), got: {:?}",
        built.codes()
    );
}

/// #11(c): a `$params`-only surface still exposes nothing (no `$view`/`$mut`), so
/// it is rejected exactly as the empty surface is.
#[test]
fn params_only_surface_rejected() {
    let built = build(&public_surface(r#""$params": { "done": "bool" }"#));
    assert!(
        built.result.is_err(),
        "a `$params`-only surface exposes nothing and must be rejected at load (§10.1)"
    );
    assert!(
        built.has_code("M-SURFACE"),
        "expected the surface diagnostic (M-SURFACE), got: {:?}",
        built.codes()
    );
}

/// Control: a `$view`-only surface exposes a read result and loads — the
/// exposing-member rule accepts a surface with a `$view` and no `$mut`.
#[test]
fn view_only_surface_loads() {
    let built = build(&public_surface(r#""$view": ".tasks { id, done }""#));
    built.expect_ok();
}

/// Control: a `$mut`-only surface exposes a call and loads — a surface with a
/// `$mut` and no `$view` mirrors the §10.2 `login` example (named mutations, no
/// read result). Uses an inline mutation program so no separately declared
/// mutation is needed.
#[test]
fn mut_only_surface_loads() {
    let built = build(&public_surface(r#""$mut": { "complete": ".tasks[@id].done = true" }"#));
    built.expect_ok();
}
