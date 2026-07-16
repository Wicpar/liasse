//! Module composition (SPEC.md §13): the grammar of `$config`, `$use`, `$deps`,
//! `$expose`, and a `$modules` space. Cross-package resolution is a runtime seam.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// §13.1/§13.5/§13.6/§13.8 — a module package's composition members load.
#[test]
fn module_composition_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$module": "acme.sales@1.0.0",
            "$config": { "currency": "text" },
            "$model": { "templates": { "$key": "id", "id": "text", "label": "text" } },
            "$use": { "company": "$parent", "$optional": { "billing": "acme.billing/customers@1" } },
            "$deps": { "tax": "acme.tax@2" },
            "$expose": { "templates": { "$view": ".templates { id, label }", "$mut": { "create": ".create_template" } } }
        }"#,
    );
    built.expect_ok();
}

/// §13.2/§13.4/§13.8 — a `$modules` space with an interface and an exposure.
#[test]
fn module_space_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.host@1.0.0", "$model": {
            "companies": {
              "$key": "id", "id": "text",
              "modules": {
                "$modules": {
                  "$expose": { "company": { "$view": ". { id }", "$mut": { "rename": ".rename" } } }
                  "$interfaces": { "templates": { "$view": { "$key": "id", "id": "text" }, "$mut": { "create({ label: text })": { "$return": "bool" } } } }
                }
              }
            }
        } }"#,
    );
    built.expect_ok();
}

/// §13.8 / §2.5 — an unknown module-space member is rejected.
#[test]
fn unknown_space_member_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.host@1.0.0", "$model": {
            "companies": {
              "$key": "id", "id": "text",
              "modules": { "$modules": { "$plugins": {} } }
            }
        } }"#,
    );
    assert!(built.has_code("M-MODULE"));
    assert!(built.points_at("$plugins"));
}

/// §13.8 — an interface member outside `$view`/`$mut` is rejected.
#[test]
fn unknown_interface_member_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.host@1.0.0", "$model": {
            "companies": {
              "$key": "id", "id": "text",
              "modules": { "$modules": { "$interfaces": { "t": { "$view": { "$key": "id", "id": "text" }, "$secret": "x" } } } }
            }
        } }"#,
    );
    assert!(built.has_code("M-MODULE"));
}

/// §13.6 — a `$deps` entry must name a package spec string.
#[test]
fn deps_non_string_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$module": "acme.x@1.0.0",
            "$deps": { "tax": { "package": "acme.tax@2" } },
            "$model": { "a": { "$key": "id", "id": "text" } }
        }"#,
    );
    assert!(built.has_code("M-MODULE"));
}
