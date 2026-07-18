//! Red-team: the §8.8 pure-position gate is NOT enforced on a *role-granted*
//! surface `$view` (only on a `$public` surface `$view`).
//!
//! SPEC.md §8.8 "Expression effects" (line 1145): "Computed fields, views,
//! `$normalize`, and `$check` use pure functions only." §16.3 (line 2750):
//! "Generated functions run in mutation/write-time positions." §8.12/§16.1
//! classify `now()`/`uuid()` as generated. "The checker rejects an effect class
//! used in the wrong position while loading the package" (§8.8, line 1147).
//!
//! §10.1: "`$view` defines its read result." §10.3 shows a *role-granted* surface
//! carrying a `$view` (`"tasks": { "$view": ".tasks[...] { ... }" }`), and §10.5
//! shows another (`"company": { "$view": ". { id, name }" }`). A role surface
//! `$view` is therefore a view exactly as a `$public` surface `$view` is, so the
//! §8.8 pure-position gate MUST apply to it identically.
//!
//! The commit c985327 fix added the generated-call rejection *inside*
//! `SurfacePhase::check_view`, but `surface.rs` only calls `check_view` when the
//! surface is `public` (`surface.rs:169  if public { self.check_view(...) }`). A
//! role-granted surface (`public == false`) never reaches `check_view`, so its
//! `$view` is parsed by nothing and purity-gated by nothing — a generated `now()`
//! slips through in a role view while the byte-identical `$public` view is
//! correctly rejected.
//!
//! All three cases below feed the byte-identical projection
//! `.tasks { id, checked_at: now() }`. `$roles` is declared at the model root, so
//! a role surface's `.` receiver is the model root — identical to the `$public`
//! surface's `.` — leaving the *declaration position* (public vs role) as the
//! only variable.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// The scaffold with the given role-granted surface `$view` text. `$roles` is at
/// the model root so the role's `.` receiver is the model root (same as a public
/// surface's `.`). A `token` authenticator and a matching `$members` make the
/// role well-formed, so the only variable under test is the view expression.
fn role_model(view: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "redteam.roleview@1.0.0",
          "$model": {{
            "accounts": {{ "$key": "id", "id": "text", "enabled": "bool = true" }},
            "tasks": {{ "$key": "id", "id": "uuid = uuid()", "done": "bool = false" }},
            "$auth": {{
              "token": {{
                "$credential": "text",
                "$verify": "$credential",
                "$actor": "/accounts[$proof]"
              }}
            }},
            "$roles": {{
              "member": {{
                "$auth": "token",
                "$members": ".accounts[:a | a.enabled]",
                "stamped": {{ "$view": "{view}" }}
              }}
            }}
          }}
        }}"#
    )
}

/// The same scaffold, but exposing the surface under `$public` instead of a role.
fn public_model(view: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "redteam.roleview@1.0.0",
          "$model": {{
            "accounts": {{ "$key": "id", "id": "text", "enabled": "bool = true" }},
            "tasks": {{ "$key": "id", "id": "uuid = uuid()", "done": "bool = false" }},
            "$public": {{
              "stamped": {{ "$view": "{view}" }}
            }}
          }}
        }}"#
    )
}

/// Control: a role-granted surface `$view` with a *pure* projection loads. This
/// proves the surrounding scaffold (root `$roles`, `token` `$auth`, `$members`,
/// the granted `stamped` surface) is well-formed, so the ONLY variable in the
/// cases below is whether the view expression calls the generated `now()`.
#[test]
fn role_surface_view_control_loads() {
    let built = build(&role_model(".tasks { id, done }"));
    built.expect_ok();
}

/// Positive control (§8.8/§16.3): the byte-identical `now()`-bearing projection,
/// declared as a `$public` surface `$view`, IS rejected as an effect-class
/// violation — the c985327 fix enforces the pure-position gate here. This
/// confirms the rule is real and enforced for the public surface view position,
/// so the role case below differs only in declaration position.
#[test]
fn public_surface_view_with_now_is_rejected() {
    let built = build(&public_model(".tasks { id, checked_at: now() }"));
    assert!(
        built.result.is_err(),
        "a $public surface `$view` calling the generated now() must be rejected (§8.8/§16.3)"
    );
    assert!(
        built.has_code("M-EXPR"),
        "expected the pure-position diagnostic (M-EXPR), got: {}",
        built.rendered()
    );
}

/// THE BUG (§8.8/§16.3): the byte-identical `now()`-bearing projection, declared
/// as a *role-granted* surface `$view`, MUST be rejected exactly as the `$public`
/// one above is — a role surface `$view` "defines its read result" (§10.1, §10.3)
/// and is therefore a view (§8.8). "The checker rejects an effect class used in
/// the wrong position while loading the package" (§8.8). Yet the package loads:
/// `surface.rs` only runs `check_view` (which holds the c985327 purity gate) for
/// `public` surfaces, so a role view is never purity-checked. This test FAILS
/// today because the generated call is admitted in a pure read position.
#[test]
fn role_surface_view_with_now_must_be_rejected() {
    let built = build(&role_model(".tasks { id, checked_at: now() }"));
    assert!(
        built.result.is_err(),
        "SPEC VIOLATION (§8.8/§16.3): a role-granted surface `$view` calling the generated \
         now() must be rejected as an effect-class violation while loading, exactly as the \
         byte-identical $public surface view is — but the model builder accepted it. The \
         c985327 purity gate lives inside `check_view`, which `surface.rs` runs only for \
         public surfaces, so a role view escapes the §8.8 pure-position check entirely."
    );
    assert!(
        built.has_code("M-EXPR"),
        "expected the pure-position diagnostic (M-EXPR) on the role view, got: {}",
        built.rendered()
    );
}
