//! Statement-scope rules for mutation and surface-inline programs:
//! local-binding threading (§8, Annex C.9), the write-to-computed-value rule
//! applied inside a surface-inline `$mut` program (§5.2/§8.5/§10.1), and the
//! public-operation `$actor`/`$session` prohibition (§10.2). Each expectation is
//! derived from the cited SPEC.md rule, not from prior implementation output.

// Tests are expected to panic on a failed assertion (AGENTS.md).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;
use liasse_model::code;

/// §8 / Annex C.9: a local introduced by `local = value_or_mutation_result` is
/// visible to later statements. Here `a` binds one selected `accounts` row and a
/// later `assert` reads `a.balance`; the program must load. (Before local-binding
/// threading the model rejected `a` as an unknown name.)
#[test]
fn local_binding_usable_in_later_statement() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.local@1.0.0"
          "$model": {
            "accounts": { "$key": "id", "id": "text", "balance": "int" }
            "$mut": { "check_it": ["a = .accounts[@id]", "assert(a.balance >= 0, 'neg')", "return a { id }"] }
          }
        }"#,
    );
    built.expect_ok();
}

/// §8: the threaded local carries the real row type, so a *wrong* field access on
/// it is still rejected — threading resolves names, it does not blanket-accept
/// them. `a.nonexistent` must fail (proving `a` is typed as an `accounts` row).
#[test]
fn local_binding_wrong_field_still_rejected() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.local2@1.0.0"
          "$model": {
            "accounts": { "$key": "id", "id": "text", "balance": "int" }
            "$mut": { "bad": ["a = .accounts[@id]", "assert(a.nonexistent >= 0, 'x')", "return a { id }"] }
          }
        }"#,
    );
    // The reference is checked against the row type, so an undeclared field is a
    // load error (E-EXPR/M-EXPR from the expression layer), not silently accepted.
    assert!(built.expect_err().iter().count() >= 1);
    assert!(built.points_at("a.nonexistent") || built.points_at("nonexistent"));
}

/// §5.2/§8.5/§10.1: a surface-inline `$mut` program (a state-changing expression
/// written directly under a surface `$mut`, not a `.name` reference) is a mutation
/// program and may not assign to a read-only computed value.
#[test]
fn surface_inline_expression_write_to_computed_rejected() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.si1@1.0.0"
          "$model": {
            "invoices": {
              "$key": "id", "id": "text", "subtotal": "int", "tax": "int",
              "total": "= .subtotal + .tax"
            }
            "$public": {
              "invoices": {
                "$view": ".invoices { id, total }"
                "$mut": {
                  "add": ".invoices + { id: @id, subtotal: @subtotal, tax: @tax }"
                  "forge_total": ".invoices[@id].total = @total"
                }
              }
            }
          }
        }"#,
    );
    assert!(built.has_code(code::SURFACE));
    assert!(built.points_at(".invoices[@id].total"));
    assert!(built.has_hint());
}

/// §5.2/§8.5/§10.1: the same rule applies to the array (multi-statement) inline
/// form, not only the single-expression form.
#[test]
fn surface_inline_array_program_write_to_computed_rejected() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.si2@1.0.0"
          "$model": {
            "invoices": {
              "$key": "id", "id": "text", "subtotal": "int", "tax": "int",
              "total": "= .subtotal + .tax"
            }
            "$public": {
              "invoices": {
                "$view": ".invoices { id, total }"
                "$mut": { "forge": [".invoices[@id].total = @total", "return .invoices[@id] { id }"] }
              }
            }
          }
        }"#,
    );
    assert!(built.has_code(code::SURFACE));
    assert!(built.points_at(".invoices[@id].total"));
}

/// A well-formed surface-inline program (an insert and a keyed patch to a
/// *writable* field) must still load: the new checks reject only genuine
/// violations, they do not route the inline form through full value typing.
#[test]
fn surface_inline_valid_program_accepted() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.si3@1.0.0"
          "$model": {
            "invoices": { "$key": "id", "id": "text", "subtotal": "int", "tax": "int" }
            "$public": {
              "invoices": {
                "$view": ".invoices { id, subtotal }"
                "$mut": {
                  "add": ".invoices + { id: @id, subtotal: @subtotal, tax: @tax }"
                  "retax": ".invoices[@id].tax = @tax"
                }
              }
            }
          }
        }"#,
    );
    built.expect_ok();
}

/// §10.2: a public operation has no `$actor`/`$session`, so a public surface-inline
/// program referencing `$actor` is statically invalid — no context can bind it.
#[test]
fn public_surface_inline_program_binds_actor_rejected() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.si4@1.0.0"
          "$model": {
            "entries": { "$key": "id", "id": "text", "who": "text" }
            "$public": {
              "log": { "$mut": { "grab": ".entries + { id: @id, who: $actor.id }" } }
            }
          }
        }"#,
    );
    assert!(built.has_code(code::SURFACE));
    assert!(built.points_at("$actor"));
    assert!(built.has_hint());
}

/// §10.2 is public-scoped: a *role* surface (authenticated) may read `$actor` in
/// its inline program, so the same reference under a role must NOT trigger the
/// public prohibition. The package below is a complete valid role.
#[test]
fn role_surface_inline_program_reads_actor_accepted() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.si5@1.0.0"
          "$requires": { "cose": "liasse.cose@1" }
          "$model": {
            "accounts": { "$key": "id", "id": "uuid = uuid()" }
            "sessions": { "$key": "id", "id": "uuid = uuid()", "account": { "$ref": "/accounts" }, "revoked": "bool = false" }
            "audits": { "$key": "id", "id": "uuid = uuid()", "who": "uuid" }
            "$auth": {
              "session": {
                "$credential": "bytes",
                "$verify": "cose.verify(/session_keys, $credential)",
                "$session": "/sessions[$proof.session]",
                "$actor": "/accounts[$session.account]",
                "$check": "$proof.auth == $auth_name"
              }
            }
            "$roles": {
              "member": {
                "$auth": "session",
                "$members": ".accounts",
                "log": { "$mut": { "record": ".audits + { who: $actor.id }" } }
              }
            }
          }
        }"#,
    );
    built.expect_ok();
}
