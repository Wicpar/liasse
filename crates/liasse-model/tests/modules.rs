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

/// §13.9 — the parent aggregates every instance exposing an interface with the
/// `.modules::iface` selector, reading `modules.$key` (the instance name),
/// `templates.$key` (the interface row key), and the declared interface fields.
/// This is the composite typing gain: the space is a keyed view of instances
/// whose interface members are nested collections of their `$view` row shape.
#[test]
fn module_space_interface_aggregation_types() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.host@1.0.0", "$model": {
            "companies": {
              "$key": "id", "id": "text",
              "modules": {
                "$modules": {
                  "$interfaces": {
                    "templates": { "$view": { "$key": "id", "id": "text", "label": "text" } }
                  }
                }
              },
              "catalog": {
                "$view": ".modules::templates { module: modules.$key, template: templates.$key, id, label, $sort: [module, template] }"
              }
            }
        } }"#,
    );
    built.expect_ok();
}

/// §13.9 — a whole-space aggregation (`count(.modules)`) type-checks because the
/// space projects to a view of instance rows, not an opaque `json`.
#[test]
fn module_space_whole_aggregation_types() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.host@1.0.0", "$model": {
            "companies": {
              "$key": "id", "id": "text",
              "modules": { "$modules": { "$interfaces": { "templates": { "$view": { "$key": "id", "id": "text" } } } } },
              "installed": { "$view": "= count(.modules)" }
            }
        } }"#,
    );
    built.expect_ok();
}

/// §13.8/§13.9 — addressing an interface the space does not declare is a static
/// type error: only declared boundary contracts are aggregatable.
#[test]
fn module_space_unknown_interface_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.host@1.0.0", "$model": {
            "companies": {
              "$key": "id", "id": "text",
              "modules": { "$modules": { "$interfaces": { "templates": { "$view": { "$key": "id", "id": "text" } } } } },
              "catalog": { "$view": ".modules::billing { id }" }
            }
        } }"#,
    );
    assert!(built.has_code("E-EXPR"));
    assert!(built.points_at(".modules::billing"));
}
