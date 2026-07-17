#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Blob placement member selectors `.$satisfied`, `.$stored`, `.$surplus`
//! (§18.5).
//!
//! §18.5: a committed blob occurrence exposes its logical placement state —
//! `$stored` (the verified stores), `$satisfied` (whether the placement policy
//! is satisfied over them), and `$surplus` (verified copies outside the
//! currently required policy). These are engine-recorded observations the pure
//! evaluator reads off the environment's placement index, so each expected value
//! is the recorded fact, never the program's own answer. The store-identity view
//! projects as `{ id }` and a `/stores['id']` row tests membership by identity
//! (§18.11).

mod common;

use common::{check, collection, eval, keyed_row, keyless_row, row_type, scalar, scell, try_eval, vtext, FixedEnv};
use liasse_expr::{BlobPlacement, Cell, EvalError, ExprType};
use liasse_value::{BlobDescriptor, MediaType, Sha512, Text, Type, Value};

/// The 128-hex canonical text of a fixed SHA-512 (64 `0xab` bytes).
fn hash_hex() -> String {
    "ab".repeat(64)
}

/// A `text/plain` descriptor for 4 bytes, the committed content under test.
fn descriptor() -> Value {
    Value::Blob(Box::new(BlobDescriptor::new(
        Sha512::parse(&hash_hex()).expect("hash"),
        4,
        MediaType::new("text/plain"),
        None,
    )))
}

/// The §18.5 facts an engine records for the committed occurrence.
fn facts(stored: &[&str], satisfied: bool, surplus: &[&str]) -> BlobPlacement {
    BlobPlacement {
        stored: stored.iter().map(|s| Text::new(*s)).collect(),
        satisfied,
        surplus: surplus.iter().map(|s| Text::new(*s)).collect(),
    }
}

/// A store-identity row type (`{ id: text }`, keyed by the id text).
fn store_row() -> liasse_expr::RowType {
    row_type(vec![("id", scalar(Type::Text))], Some(scalar(Type::Text)))
}

/// A scope whose current row exposes a `file` blob field.
fn scope() -> common::FixedScope {
    common::FixedScope::new(ExprType::Row(row_type(vec![("file", scalar(Type::Blob))], None)))
}

/// A scope whose current row exposes both the blob `file` and a keyed `stores`
/// collection, so `/stores['id'] in .file.$stored` (§18.11) type-checks.
fn membership_scope() -> common::FixedScope {
    common::FixedScope::new(ExprType::Row(row_type(
        vec![("file", scalar(Type::Blob)), ("stores", ExprType::View(store_row()))],
        None,
    )))
}

/// A `.` row carrying the blob `file` cell.
fn world() -> Cell {
    Cell::Row(Box::new(keyless_row(0, vec![("file", scell(descriptor()))])))
}

/// A `.` row carrying the blob `file` cell and a `stores` collection of two
/// store rows keyed `a` and `b`.
fn membership_world() -> Cell {
    let stores = collection(vec![
        keyed_row("a", vtext("a"), vec![("id", scell(vtext("a")))]),
        keyed_row("b", vtext("b"), vec![("id", scell(vtext("b")))]),
    ]);
    Cell::Row(Box::new(keyless_row(0, vec![("file", scell(descriptor())), ("stores", stores)])))
}

/// The env root is unused by these field reads but required by the evaluator.
fn root() -> liasse_expr::Row {
    keyless_row(0, vec![])
}

/// §18.5 typing: `$satisfied` is `bool`; `$stored`/`$surplus` are views of
/// store-identity rows keyed by the store id `text`.
#[test]
fn placement_members_type_as_declared() {
    let scope = scope();
    assert_eq!(check(&scope, ".file.$satisfied").ty(), &ExprType::scalar(Type::Bool));
    for member in [".file.$stored", ".file.$surplus"] {
        let typed = check(&scope, member);
        let row = typed
            .ty()
            .as_view()
            .unwrap_or_else(|| panic!("`{member}` should type as a view"));
        assert_eq!(row.field("id"), Some(&ExprType::scalar(Type::Text)));
        assert_eq!(row.key(), Some(&ExprType::scalar(Type::Text)));
    }
}

/// §18.5: `$satisfied` reads the recorded policy-satisfaction bool. A satisfied
/// policy reads `true`; an unsatisfied one reads `false`.
#[test]
fn satisfied_reads_the_recorded_policy_bool() {
    let satisfied = FixedEnv::new(root()).placement(&hash_hex(), facts(&["a"], true, &[]));
    assert_eq!(
        eval(&scope(), &satisfied, &world(), ".file.$satisfied").as_scalar(),
        Some(&Value::Bool(true)),
    );
    let unsatisfied = FixedEnv::new(root()).placement(&hash_hex(), facts(&[], false, &[]));
    assert_eq!(
        eval(&scope(), &unsatisfied, &world(), ".file.$satisfied").as_scalar(),
        Some(&Value::Bool(false)),
    );
}

/// §18.5: `$stored` lists the verified holding stores as store-identity rows, so
/// `.file.$stored { id }` yields each verified store's id.
#[test]
fn stored_lists_the_verified_holding_stores() {
    let env = FixedEnv::new(root()).placement(&hash_hex(), facts(&["a", "b"], true, &[]));
    let result = eval(&scope(), &env, &world(), ".file.$stored { id }");
    assert_eq!(common::ids(&result, "id"), vec![vtext("a"), vtext("b")]);
}

/// §18.5: `$surplus` lists only the verified copies outside the currently
/// required policy — here the orphaned copy in `b`, not the still-required `a`.
#[test]
fn surplus_lists_copies_outside_required_policy() {
    let env = FixedEnv::new(root()).placement(&hash_hex(), facts(&["a", "b"], true, &["b"]));
    let result = eval(&scope(), &env, &world(), ".file.$surplus { id }");
    assert_eq!(common::ids(&result, "id"), vec![vtext("b")]);
}

/// §18.11 billing pattern: `/stores['id'] in .file.$stored` tests store membership
/// by identity. A store that holds a verified copy is a member; one that does not
/// is not.
#[test]
fn store_row_membership_tests_stored_by_identity() {
    let scope = membership_scope();
    let env = FixedEnv::new(root()).placement(&hash_hex(), facts(&["a"], true, &[]));
    assert_eq!(
        eval(&scope, &env, &membership_world(), ".stores['a'] in .file.$stored").as_scalar(),
        Some(&Value::Bool(true)),
    );
    assert_eq!(
        eval(&scope, &env, &membership_world(), ".stores['b'] in .file.$stored").as_scalar(),
        Some(&Value::Bool(false)),
    );
}

/// A placement member is engine-recorded state, so reading one against an
/// environment that owns no placement for the occurrence is a typed contract
/// breach ([`EvalError::NoBlobPlacement`]) — proof the value comes from the index,
/// not the evaluator.
#[test]
fn placement_read_without_index_is_a_typed_breach() {
    let env = FixedEnv::new(root());
    let error = try_eval(&scope(), &env, &world(), ".file.$satisfied")
        .expect_err("a placement read with no recorded index must fail");
    assert_eq!(error, EvalError::NoBlobPlacement);
}
