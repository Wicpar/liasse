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

/// §20.1 — a collection MAY carry `$from` to rename an old collection, adopting
/// its rows ("The same shorthand renames a collection"). The collection-level
/// `$from` marker must load, not be rejected as an unknown reserved member;
/// the two-model rename is a runtime concern the model does not evaluate.
#[test]
fn collection_from_rename_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.mig.coll@2.0.0",
            "$model": {
                "clients": { "$from": "customers", "$key": "id", "id": "text", "name": "text" },
                "$public": { "clients": { "$view": ".clients { id, name }" } }
            }
        }"#,
    );
    built.expect_ok();
}

/// §20.1/§20.2 — the expanded-field mapping form `{ $type, $from, $as, $back }`
/// loads: the mapping members are accepted structurally and typed by the
/// (runtime) migration phase, so a target field that adopts and transforms an
/// old field is not statically rejected.
#[test]
fn field_from_as_back_mapping_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.mig.field@2.0.0",
            "$model": {
                "rows": {
                    "$key": "id",
                    "id": "text",
                    "encoded": { "$type": "text", "$from": "name", "$as": "base64.encode(string.bytes(.))", "$back": "string.from_bytes(base64.decode(.))" }
                }
            }
        }"#,
    );
    built.expect_ok();
}
