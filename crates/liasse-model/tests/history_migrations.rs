//! History policy (SPEC.md §19.3) and migrations (§20): declaration shapes.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// §19.3 — the `all` and `{ $minimum }` history forms load.
#[test]
fn history_forms_load() {
    for history in [r#""all""#, r#"{ "$minimum": "P10Y" }"#] {
        let src = format!(
            r#"{{ "$liasse": 1, "$app": "t.h@1.0.0", "$history": {history}, "$model": {{ "a": {{ "$key": "id", "id": "text" }} }} }}"#
        );
        build(&src).expect_ok();
    }
}

/// §19.3 — a string `$history` other than `all` is rejected.
#[test]
fn history_bad_string_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.h@1.0.0", "$history": "recent", "$model": { "a": { "$key": "id", "id": "text" } } }"#,
    );
    assert!(built.has_code("M-HISTORY"));
    assert!(built.has_hint());
}

/// §19.3 — an object `$history` requires `$minimum`.
#[test]
fn history_object_requires_minimum() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.h@1.0.0", "$history": { "$keep": "P1Y" }, "$model": { "a": { "$key": "id", "id": "text" } } }"#,
    );
    assert!(built.has_code("M-HISTORY"));
}

/// §20.1 — a package-level migration program keyed by an exact source version.
#[test]
fn migration_program_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.mig@2.0.0",
            "$model": { "people": { "$key": "id", "id": "text", "name": "text" } },
            "$migrations": { "1.4.0": [".people = $old.users { id, name: string.trim(.name) }"] }
        }"#,
    );
    built.expect_ok();
}

/// §20.1 — a migration key that is not an exact version is rejected.
#[test]
fn migration_bad_version_key_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.mig@2.0.0",
            "$model": { "a": { "$key": "id", "id": "text" } },
            "$migrations": { "^1.0": [".a = $old.a"] }
        }"#,
    );
    assert!(built.has_code("M-MIGRATE"));
    assert!(built.has_hint());
}

/// §20.1 — a migration program must be a non-empty array of statements.
#[test]
fn migration_empty_program_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.mig@2.0.0",
            "$model": { "a": { "$key": "id", "id": "text" } },
            "$migrations": { "1.0.0": [] }
        }"#,
    );
    assert!(built.has_code("M-MIGRATE"));
}
