#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Keyring public version selectors over a keyring's version view (§17.2).
//!
//! A keyring exposes its managed versions as a view of version-metadata rows.
//! `.$current` selects the single active version (§17.3: at most one is active,
//! so it types as a row); `.$accepted` and `.$public` select the versions still
//! accepted for verification (active plus retired, §17.1/§17.3); `.$versions`
//! selects every retained version. Lifecycle resolution is the environment's
//! keyring index (a test index reading each version's `state` cell), so
//! evaluation stays pure. Expected version sets are deduced from §17.3's
//! lifecycle: active is selected/current, retired remains accepted,
//! revoked/destroyed are rejected.

mod common;

use common::{
    check, check_rejects, collection, eval, ids, keyless_row, keyed_row, row_type, scalar, scell,
    vtext, view, FixedEnv, FixedScope,
};
use liasse_expr::{Cell, ExprType, Row};
use liasse_value::Type;

/// A keyring version row keyed by its id, carrying its lifecycle `state`.
fn version(id: &str, state: &str) -> Row {
    keyed_row(
        id,
        vtext(id),
        vec![("id", scell(vtext(id))), ("state", scell(vtext(state)))],
    )
}

/// A scope whose root exposes `session_keys` as a view of version rows.
fn scope() -> FixedScope {
    let ty = row_type(
        vec![("id", scalar(Type::Text)), ("state", scalar(Type::Text))],
        Some(scalar(Type::Text)),
    );
    let root_ty = row_type(vec![("session_keys", view(ty))], None);
    FixedScope::new(ExprType::Row(root_ty))
}

/// A world of four versions spanning the full lifecycle (§17.3).
fn lifecycle_world() -> (Cell, Row) {
    let versions = vec![
        version("v1", "destroyed"),
        version("v2", "revoked"),
        version("v3", "retired"),
        version("v4", "active"),
    ];
    let root = keyless_row(0, vec![("session_keys", collection(versions))]);
    (Cell::Row(Box::new(root.clone())), root)
}

/// §17.2/§17.3: `.$current` is the single active version. It types as a row, so
/// a field read resolves against that one version.
#[test]
fn current_reads_the_single_active_version() {
    let (dot, root) = lifecycle_world();
    let result = eval(&scope(), &FixedEnv::new(root), &dot, ".session_keys.$current.id");
    assert_eq!(result.as_scalar(), Some(&vtext("v4")));
}

/// §17.2 typing: `.$current` is one row, not a stream — a projection block over
/// it is not a view read.
#[test]
fn current_types_as_a_row() {
    let typed = check(&scope(), ".session_keys.$current");
    assert!(matches!(typed.ty(), ExprType::Row(_)));
}

/// §17.1/§17.3: `.$accepted` is the active version plus every retired version
/// still accepted for verification; revoked and destroyed versions are excluded.
#[test]
fn accepted_excludes_revoked_and_destroyed() {
    let (dot, root) = lifecycle_world();
    let result = eval(&scope(), &FixedEnv::new(root), &dot, ".session_keys.$accepted { id }");
    assert_eq!(ids(&result, "id"), vec![vtext("v3"), vtext("v4")]);
}

/// §17.2: `.$public` covers the same accepted versions as `.$accepted`.
#[test]
fn public_covers_accepted_versions() {
    let (dot, root) = lifecycle_world();
    let result = eval(&scope(), &FixedEnv::new(root), &dot, ".session_keys.$public { id }");
    assert_eq!(ids(&result, "id"), vec![vtext("v3"), vtext("v4")]);
}

/// §17.2: `.$versions` exposes every retained version, independent of lifecycle
/// state.
#[test]
fn versions_exposes_every_retained_version() {
    let (dot, root) = lifecycle_world();
    let result = eval(&scope(), &FixedEnv::new(root), &dot, ".session_keys.$versions { id }");
    assert_eq!(
        ids(&result, "id"),
        vec![vtext("v1"), vtext("v2"), vtext("v3"), vtext("v4")],
    );
}

/// §17.3: at most one version is active, so `.$current` with no active version
/// is a cardinality failure rather than a silent empty read.
#[test]
fn current_without_active_version_fails_cardinality() {
    let versions = vec![version("v1", "retired"), version("v2", "revoked")];
    let root = keyless_row(0, vec![("session_keys", collection(versions))]);
    let dot = Cell::Row(Box::new(root.clone()));
    let typed = check(&scope(), ".session_keys.$current.id");
    let err = typed.evaluate(&FixedEnv::new(root), &dot).expect_err("no active version");
    assert_eq!(err.message(), "the active keyring version requires exactly one row, found 0");
}

/// A keyring selector applies to a view, not a scalar.
#[test]
fn keyring_over_non_view_rejects() {
    let scope = FixedScope::new(ExprType::scalar(Type::Int));
    let diags = check_rejects(&scope, "1.$current");
    assert!(diags.iter().any(|d| d.message().contains("keyring selector")));
    assert!(diags.iter().any(|d| d.message().contains("view")));
}

/// An unknown `.$name` structural selector is neither temporal nor keyring and
/// is rejected with a diagnostic naming both selector families.
#[test]
fn unknown_structural_selector_rejects() {
    let diags = check_rejects(&scope(), ".session_keys.$bogus");
    assert!(diags.iter().any(|d| {
        let m = d.message();
        m.contains("$bogus") && m.contains("$all") && m.contains("$current")
    }));
}
