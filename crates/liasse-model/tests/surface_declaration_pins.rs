//! §10.1 surface well-formedness pins (SPEC-ISSUES #10 and #11(c)).
//!
//! #10 — a surface `$view` parameter IS inferred, exactly as a mutation
//! parameter is (§8.3): every typed use of `@name` contributes a constraint and
//! all uses must agree on one type. A parameter the expression does not
//! constrain to a unique type is a static load error whose diagnostic requests an
//! explicit `$params` declaration, and an explicit `$params` entry stays
//! authoritative — a use incompatible with it is a conflict. The ROLE `$view`
//! path skips full typing for the `$actor` seam, so the model must run inference
//! (and report both failures) there itself. Both paths must agree.
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

// --- #10: surface `$view` parameters are inferred, fail-to-explicit --------

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

/// #10: a ROLE surface `$view` reading `@done` with no `$params` entry LOADS —
/// the comparison `t.done == @done` against the `bool` field anchors it, so
/// §8.3 inference settles `@done: bool` (§10.1). This is the path the rule bites
/// on: a role `$view` skips full typing (the `$actor` seam), so inference has to
/// run explicitly there.
#[test]
fn role_view_inferred_parameter_loads() {
    let built = build(&role_surface(r#""$view": ".tasks[:t | t.done == @done] { id }""#));
    built.expect_ok();
}

/// #10: the byte-identical PUBLIC surface `$view` loads too — both paths run the
/// same inference over the same expression and reach the same contract.
#[test]
fn public_view_inferred_parameter_loads() {
    let built = build(&public_surface(r#""$view": ".tasks[:t | t.done == @done] { id }""#));
    built.expect_ok();
}

/// #10: §10.1's third anchor, "a typed function argument". `@q`'s ONLY use is the
/// argument of the core built-in `string.lower` — it is not a direct comparison
/// operand (the comparison's operand is the call, not the parameter) and not a
/// key selector — so the built-in's pinned `string.lower(text)` signature is what
/// types it. A database-evaluated position admits only built-ins (§16.5), so this
/// is decidable with no host resolution.
#[test]
fn public_view_builtin_argument_parameter_loads() {
    let built = build(&public_surface(
        r#""$view": ".tasks[:t | t.id == string.lower(@q)] { id }""#,
    ));
    built.expect_ok();
}

/// The role path anchors a built-in argument identically (it skips full typing
/// for the `$actor` seam, so inference is the only thing that can type `@q`).
#[test]
fn role_view_builtin_argument_parameter_loads() {
    let built = build(&role_surface(
        r#""$view": ".tasks[:t | t.id == string.lower(@q)] { id }""#,
    ));
    built.expect_ok();
}

/// §6.5 + §10.1: an arity-2 core built-in anchors EVERY argument position, not
/// just the first. `string.contains(subject, needle)` takes two `text` operands,
/// and `@subject`/`@needle` are used nowhere else, so inference settles both and
/// the surface loads with no `$params` at all. This is the pin that the search
/// predicates (`starts_with`/`ends_with`/`contains`) anchor through the same
/// roster the checker resolves them with — a second, arity-blind table would
/// leave the needle uninferable here.
#[test]
fn view_arity_two_builtin_arguments_are_both_inferred() {
    let view = r#""$view": ".tasks[:t | string.contains(@subject, @needle)] { id }""#;
    build(&public_surface(view)).expect_ok();
    // The role path skips full typing (the `$actor` seam), so its load proves
    // inference alone — not the type checker — typed both arguments.
    build(&role_surface(view)).expect_ok();
}

/// The type each of those positions pins is `text`, in BOTH slots: declaring
/// either one as `bool` is the §8.3 "all uses MUST agree on one type" conflict.
/// Pinned on the ROLE path, where inference is the only thing that can catch it.
#[test]
fn view_arity_two_builtin_arguments_are_both_text() {
    for declared in ["subject", "needle"] {
        let built = build(&role_surface(&format!(
            r#""$params": {{ "{declared}": "bool" }}, "$view": ".tasks[:t | string.contains(@subject, @needle)] {{ id }}""#
        )));
        assert!(
            built.result.is_err(),
            "declaring `@{declared}` as `bool` contradicts the `text` \
             `string.contains` pins at its position (§6.5, §10.1)"
        );
        assert!(
            built.has_code("M-SURFACE"),
            "expected the surface diagnostic (M-SURFACE) for `@{declared}`, got: {:?}",
            built.codes()
        );
        assert!(
            built.rendered().contains("two incompatible types"),
            "the diagnostic must name the type conflict for `@{declared}`, got: {}",
            built.rendered()
        );
    }
}

/// A built-in slot that pins no single type contributes NO constraint: `size(x)`
/// accepts text, bytes, a set, or a collection, so a parameter whose only use is
/// `size(@q)` stays uninferable and keeps the explicit-declaration error.
#[test]
fn public_view_generic_builtin_argument_stays_uninferable() {
    let built = build(&public_surface(r#""$view": ".tasks[:t | size(@q) > 0] { id }""#));
    assert!(
        built.result.is_err(),
        "`size` admits several argument types, so it constrains nothing (§10.1)"
    );
    assert!(
        built.rendered().contains("declare it in `$params` with its type"),
        "a generic built-in slot must still request an explicit declaration, got: {}",
        built.rendered()
    );
}

/// #10 fail-to-explicit: a parameter no typed position anchors — `@tag` is only
/// ever a projection member, and `@other` is only ever compared to `@tag` — is a
/// static load error, and the diagnostic MUST request the explicit declaration
/// §10.1 names ("declare the parameter in `$params` with its type").
#[test]
fn public_view_unanchored_parameter_rejected_asking_for_params() {
    let built =
        build(&public_surface(r#""$view": ".tasks[:t | @other == @tag] { id, tag: @tag }""#));
    assert!(
        built.result.is_err(),
        "a surface `$view` parameter no typed position constrains must be rejected (§10.1)"
    );
    assert!(
        built.has_code("M-SURFACE"),
        "expected the surface diagnostic (M-SURFACE), got: {:?}",
        built.codes()
    );
    let text = built.rendered();
    assert!(
        text.contains("declare it in `$params` with its type"),
        "the diagnostic must request an explicit `$params` declaration (§10.1), got: {text}"
    );
    assert!(
        text.contains("does not constrain to a single type"),
        "the diagnostic must say the expression pins no single type (§10.1), got: {text}"
    );
}

/// #10 fail-to-explicit, ROLE path: the same unanchored parameter is rejected
/// with the same explicit-declaration request even though a role `$view` is not
/// fully typed (the `$actor` seam). Both paths agree.
#[test]
fn role_view_unanchored_parameter_rejected_asking_for_params() {
    let built = build(&role_surface(r#""$view": ".tasks[:t | @other == @tag] { id, tag: @tag }""#));
    assert!(
        built.result.is_err(),
        "a role surface `$view` parameter no typed position constrains must be rejected (§10.1)"
    );
    let text = built.rendered();
    assert!(
        text.contains("declare it in `$params` with its type"),
        "the role path must request an explicit `$params` declaration too, got: {text}"
    );
}

/// #10: an explicit `$params` entry stays authoritative — a use incompatible with
/// it is the §8.3 "all uses MUST agree on one type" conflict, a static load
/// error. Pinned on the ROLE path, which skips full typing, so the conflict is
/// caught by inference rather than by the type checker.
#[test]
fn role_view_declared_parameter_conflicting_with_use_rejected() {
    let built = build(&role_surface(
        r#""$params": { "done": "text" }, "$view": ".tasks[:t | t.done == @done] { id }""#,
    ));
    assert!(
        built.result.is_err(),
        "a `$params` type the view's own use contradicts must be rejected (§10.1, §8.3)"
    );
    assert!(
        built.has_code("M-SURFACE"),
        "expected the surface diagnostic (M-SURFACE), got: {:?}",
        built.codes()
    );
    assert!(
        built.rendered().contains("two incompatible types"),
        "the diagnostic must name the type conflict, got: {}",
        built.rendered()
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
/// undeclared `@plan` LOADS — the `$bind` candidate row is typed, so
/// `child.plan != @plan` anchors `@plan` to `text` exactly as a `$view` filter
/// would (§10.1, §10.5).
#[test]
fn recursive_where_inferred_parameter_loads() {
    let built = build(&recursive_where("child.plan != @plan", ""));
    built.expect_ok();
}

/// #10 (third path): a `$recursive` predicate anchors a built-in argument too —
/// `@plan`'s only use is `string.lower(@plan)`, typed `text` by the pinned
/// signature (§16.1), so the coverage block loads.
#[test]
fn recursive_where_builtin_argument_parameter_loads() {
    let built = build(&recursive_where("child.plan != string.lower(@plan)", ""));
    built.expect_ok();
}

/// #10 fail-to-explicit in a `$recursive` predicate: comparing two parameters
/// anchors neither, so the predicate is rejected asking for a `$params`
/// declaration — the same rule and the same diagnostic as a `$view`.
#[test]
fn recursive_where_unanchored_parameter_rejected_asking_for_params() {
    let built = build(&recursive_where("@a != @b", ""));
    assert!(
        built.result.is_err(),
        "a `$recursive` predicate parameter no typed position constrains must be rejected (§10.1)"
    );
    assert!(
        built.rendered().contains("declare it in `$params` with its type"),
        "the predicate diagnostic must request an explicit `$params` declaration, got: {}",
        built.rendered()
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
