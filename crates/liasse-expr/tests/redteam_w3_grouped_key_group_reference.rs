#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! WAVE-3 re-challenge of the §7 grouping binding-frame fix (commit 8d79b69).
//!
//! AREA 1(a): a synthetic `$key` component that reads `group` itself, e.g.
//! `.lines { $key: [size], size: count(group) }`.
//!
//! §7.2 (verbatim): "Rows sharing the synthetic key form one group. `group` is
//! the source-row view for that output row." The group is DEFINED as the rows
//! sharing the synthetic key, so the key is logically PRIOR to the group: the
//! group does not exist until the key has partitioned the rows. A `$key`
//! component that reads `group` is therefore circular — §7.1 (verbatim):
//! "They MAY refer to one another when their dependency graph is acyclic" — and
//! a `key -> group -> key` dependency is a cycle, so the declaration is
//! statically ill-formed and MUST be rejected at load.
//!
//! The impl instead ACCEPTS it: `check/project.rs` binds `group` for EVERY
//! output (keys included, `if grouped { self.bind("group", ...) }`), so
//! `count(group)` type-checks in a key output. Then at read, `eval/views.rs`
//! `group_key` pushes the projection frame with `push_project_frame(scope,
//! None)` — deliberately NO `group` binding, because the group cannot exist yet
//! — so evaluating the key output `count(group)` reads an unbound name and the
//! whole grouped read FAULTS with `EvalError::UnboundName { name: "group" }`
//! (documented as an "environment/host contract breach, not authoring error").
//!
//! A load-accepted view that faults at read with an internal unbound-binding
//! error is neither a SPEC-sanctioned load nor a SPEC-sanctioned read result:
//! impl != SPEC. Root cause: the checker over-accepts (`check/project.rs` binds
//! `group` unconditionally for all outputs, ~L138-141), while the evaluator's
//! `group_key` (`eval/views.rs` ~L357-358) correctly has no `group` in scope.

mod common;

use common::{
    check, check_rejects, collection, eval, keyless_row, row, row_type, rows_fields, scalar, scell,
    try_eval, view, vint, vtext, FixedEnv, FixedScope,
};
use liasse_expr::{Cell, EvalError, ExprType};
use liasse_value::Type;

/// A `lines` collection with `account` (text) and `debit` (int) source fields.
fn lines_scope() -> (FixedScope, FixedEnv, Cell) {
    let ty = row_type(
        vec![
            ("id", scalar(Type::Text)),
            ("account", scalar(Type::Text)),
            ("debit", scalar(Type::Int)),
        ],
        Some(scalar(Type::Text)),
    );
    let rows = vec![
        row(1, vtext("l1"), vec![("id", scell(vtext("l1"))), ("account", scell(vtext("a"))), ("debit", scell(vint(10)))]),
        row(2, vtext("l2"), vec![("id", scell(vtext("l2"))), ("account", scell(vtext("a"))), ("debit", scell(vint(5)))]),
        row(3, vtext("l3"), vec![("id", scell(vtext("l3"))), ("account", scell(vtext("b"))), ("debit", scell(vint(3)))]),
    ];
    let root_ty = row_type(vec![("lines", view(ty))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));
    let root = keyless_row(0, vec![("lines", collection(rows))]);
    let dot = Cell::Row(Box::new(root.clone()));
    (scope, FixedEnv::new(root), dot)
}

/// CONTROL — `count(group)` in a NON-key output loads and reads correctly: the
/// group is bound in `project_row`, so `n: count(group)` yields each group's
/// size. Two groups (`a` with two rows, `b` with one), in synthetic-key order.
/// This proves the grouping machinery and the `group` binding are sound for the
/// output frame; only the key-partition frame is at issue below.
#[test]
fn control_count_group_in_nonkey_output_reads() {
    let (scope, env, dot) = lines_scope();
    let source = r#".lines { $key: [acct], acct: account, n: count(group) }"#;
    let result = eval(&scope, &env, &dot, source);
    assert_eq!(
        rows_fields(&result),
        vec![
            vec![("acct".to_owned(), vtext("a")), ("n".to_owned(), vint(2))],
            vec![("acct".to_owned(), vtext("b")), ("n".to_owned(), vint(1))],
        ],
    );
}

/// FINDING — a `$key` component reading `group` (`$key: [size], size:
/// count(group)`) is statically circular and MUST be rejected at load
/// (§7.1 acyclic / §7.2 group-is-formed-by-the-key). This assertion FAILS today
/// because the checker accepts it (it binds `group` for key outputs too), then
/// the read faults with an unbound-`group` error. When the checker is fixed to
/// reject a key that reads `group`, this passes.
#[test]
fn key_component_referencing_group_is_rejected_at_load() {
    let (scope, _env, _dot) = lines_scope();
    let source = r#".lines { $key: [size], size: count(group) }"#;
    let _diags = check_rejects(&scope, source);
}

/// DIAGNOSTIC — documents the observable defect precisely: the checker ACCEPTS
/// the circular key (so `check` does not panic), and the read then faults with
/// `EvalError::UnboundName { name: "group" }`. This is not asserting the fix
/// (it will be superseded once the checker rejects at load); it pins the exact
/// impl behavior the finding is about. Kept `#[ignore]` so it neither gates CI
/// nor breaks after the load-rejection fix removes the runtime path.
#[test]
#[ignore = "documents current buggy behavior; superseded by the load-rejection fix"]
fn key_component_referencing_group_currently_faults_at_read() {
    let (scope, env, dot) = lines_scope();
    let source = r#".lines { $key: [size], size: count(group) }"#;
    // The checker accepts it (no panic here) — that is the over-acceptance.
    let _typed = check(&scope, source);
    let err = try_eval(&scope, &env, &dot, source)
        .expect_err("a key reading `group` must not compute a result");
    assert!(
        matches!(&err, EvalError::UnboundName { name, .. } if name == "group"),
        "expected an unbound-`group` read fault, got: {}",
        err.message(),
    );
}
