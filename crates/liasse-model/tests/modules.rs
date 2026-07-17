//! Module composition (SPEC.md §13): the grammar of `$config`, `$use`, `$deps`,
//! `$expose`, and a `$modules` space. Cross-package resolution is a runtime seam.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// §13.1 — a module package's `$config` struct schema is retained on the model
/// so the composition runtime can type-check installation values against it. The
/// schema exposes each declared member's type (a supplied value that does not
/// match is thereby catchable) and its default (a member with a default MAY be
/// omitted at install; one without is required).
#[test]
fn module_config_schema_retained() {
    let built = build(
        r#"{ "$liasse": 1, "$module": "t.tplc@1.0.0",
            "$config": { "currency": "text = 'USD'", "region": "text" },
            "$model": { "templates": { "$key": "id", "id": "text" } }
        }"#,
    );
    let model = built.expect_ok();
    let schema = model.config_schema().expect("a module `$config` schema is retained");
    // The declared member type is exposed, so an install value of the wrong type
    // (e.g. a `bool` against `text`) is catchable against it (§13.1/§13.3).
    assert_eq!(schema.member_type("currency").map(|t| t.describe()), Some("text".to_owned()));
    assert_eq!(schema.member_type("region").map(|t| t.describe()), Some("text".to_owned()));
    // An undeclared member has no type — the check an install uses to reject an
    // unknown `$config` member (§13.1).
    assert!(schema.member_type("tax_id").is_none());
    // `currency` declares a default, so an install MAY omit it; `region` does not.
    assert!(schema.default("currency").is_some());
    assert!(schema.default("region").is_none());
    assert_eq!(schema.members().count(), 2);
}

/// §13.1 — an application package declares no `$config`, so its schema is `None`.
#[test]
fn application_has_no_config_schema() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.app@1.0.0",
            "$model": { "a": { "$key": "id", "id": "text" } }
        }"#,
    );
    assert!(built.expect_ok().config_schema().is_none());
}

/// §13.1 — a module's own expressions read `$config` through the binding: an
/// exposed `$view` projecting `$config.currency` type-checks against the declared
/// struct (the shape the `module-config-values-read-through-binding` scenario
/// depends on to load its child package).
#[test]
fn config_read_through_in_expose_view_types() {
    let built = build(
        r#"{ "$liasse": 1, "$module": "t.tplc@1.0.0",
            "$config": { "currency": "text = 'USD'" },
            "$model": { "templates": { "$key": "id", "id": "text", "label": "text" } },
            "$expose": { "templates": { "$view": ".templates { id, label, currency: $config.currency }" } }
        }"#,
    );
    built.expect_ok();
}

/// §13.1 — a module's own model expression reads `$config`: a computed value
/// `= $config.rate` type-checks against the declared member's type.
#[test]
fn config_read_through_in_computed_value_types() {
    let built = build(
        r#"{ "$liasse": 1, "$module": "t.tax@1.0.0",
            "$config": { "rate": "decimal = 0.2" },
            "$model": { "current_rate": "= $config.rate" }
        }"#,
    );
    built.expect_ok();
}

/// §13.1 — reading a member the `$config` struct does not declare is a static
/// type error, the module-side analogue of an install supplying an unknown
/// `$config` member.
#[test]
fn config_read_of_undeclared_member_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$module": "t.tplc@1.0.0",
            "$config": { "currency": "text = 'USD'" },
            "$model": { "templates": { "$key": "id", "id": "text", "label": "text" } },
            "$expose": { "templates": { "$view": ".templates { id, label, cur: $config.nope }" } }
        }"#,
    );
    assert!(built.has_code("E-EXPR"), "expected a config-member type error, got: {}", built.rendered());
    assert!(built.points_at("nope"));
}

/// §13.1 — a `$config` member with a malformed type is not a valid struct field;
/// the declaration is rejected (the static "valid struct type" check).
#[test]
fn config_invalid_member_type_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$module": "t.bad@1.0.0",
            "$config": { "currency": "notatype" },
            "$model": { "a": { "$key": "id", "id": "text" } }
        }"#,
    );
    assert!(built.has_code("M-TYPE"), "expected a type rejection, got: {}", built.rendered());
}

/// §13.1 — a `$config` member is a typed installation *value*, not a view or a
/// keyed collection; a `$view` member is rejected as not a valid struct field.
#[test]
fn config_non_value_member_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$module": "t.bad@1.0.0",
            "$config": { "feed": { "$view": ".a { id }" } },
            "$model": { "a": { "$key": "id", "id": "text" } }
        }"#,
    );
    assert!(built.has_code("M-MODULE"), "expected a config-shape rejection, got: {}", built.rendered());
    assert!(built.points_at("feed"));
}

/// §13.1/§13.5/§13.6/§13.8 — a module package's composition members load.
#[test]
fn module_composition_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$module": "acme.sales@1.0.0",
            "$config": { "currency": "text" },
            "$model": {
              "templates": { "$key": "id", "id": "text", "label": "text" },
              "$mut": { "create_template": ".templates + { id: @id, label: @label }" }
            },
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

/// §13.8 — a parent aggregation over an interface may project only the members
/// the interface `$view` contract declares. The `templates` interface exposes
/// `{ id, label }`; projecting `secret` (a member private to the child, absent
/// from the boundary contract) is rejected against the interface view's row
/// shape. This needs only the host package's own `$interfaces` declaration, so
/// it is a single-package static check.
#[test]
fn interface_projection_of_unbound_field_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.mod.host@1.0.0", "$model": {
            "companies": {
              "$key": "id", "id": "text", "name": "text",
              "modules": { "$modules": { "$interfaces": {
                "templates": { "$view": { "$key": "id", "id": "text", "label": "text" } }
              } } },
              "catalog": { "$view": ".modules::templates { module: modules.$key, id, label, secret, $sort: [module, id] }" }
            }
        } }"#,
    );
    assert!(built.has_code("E-EXPR"), "expected a projection rejection, got: {}", built.rendered());
    assert!(built.points_at("secret"));
}

/// §13.8 — an exposed `$view` is typed against the module's own root: a
/// projection of a field the module does not declare is a static type error, so
/// a malformed boundary contract is caught in the child package itself.
#[test]
fn expose_view_over_undeclared_field_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$module": "acme.notes@1.0.0",
            "$model": { "notes": { "$key": "id", "id": "text", "body": "text" } },
            "$expose": { "feed": { "$view": ".notes { id, missing }" } }
        }"#,
    );
    assert!(built.has_code("E-EXPR"), "expected an exposed-view type error, got: {}", built.rendered());
    assert!(built.points_at("missing"));
}

/// §13.8 — an `$expose` `$mut` binding must name a mutation the module declares;
/// binding a contract to an undeclared private mutation is rejected.
#[test]
fn expose_mut_binding_to_undeclared_mutation_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$module": "acme.notes@1.0.0",
            "$model": { "notes": { "$key": "id", "id": "text", "body": "text" } },
            "$expose": { "feed": { "$view": ".notes { id, body }", "$mut": { "add": ".no_such_mut" } } }
        }"#,
    );
    assert!(built.has_code("M-MODULE"), "expected a binding rejection, got: {}", built.rendered());
    assert!(built.points_at("no_such_mut"));
}

/// §13.8 — a well-formed `$expose` whose `$view` projects declared fields and
/// whose `$mut` binds a declared mutation loads and is captured.
#[test]
fn expose_well_formed_captured() {
    let built = build(
        r#"{ "$liasse": 1, "$module": "acme.notes@1.0.0",
            "$model": {
              "notes": { "$key": "id", "id": "text", "body": "text" },
              "$mut": { "add": ".notes + { id: @id, body: @body }" }
            },
            "$expose": { "feed": { "$view": ".notes { id, body }", "$mut": { "post": ".add" } } }
        }"#,
    );
    let model = built.expect_ok();
    let exposed = model.exposed_interfaces();
    assert_eq!(exposed.len(), 1, "one interface captured");
    let feed = exposed.first().expect("one interface");
    assert_eq!(feed.name.as_str(), "feed");
    assert!(feed.view.is_some(), "the $view is captured for the runtime");
    assert_eq!(feed.muts.len(), 1);
    assert_eq!(feed.muts.first().expect("one mut").name.as_str(), "post");
}

/// §13.11 — a surface may bind an interface boundary member
/// (`.modules[k]::templates.create`) but not dot past the instance boundary into
/// a child's private model `$mut` (`.modules[k].create_template`). The private
/// child mutation name is not a declared mutation reachable from the host, so the
/// surface binding is rejected. Single-package: only the host's declarations are
/// consulted.
#[test]
fn surface_binding_into_private_child_path_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.mod.host@1.0.0", "$model": {
            "companies": {
              "$key": "id", "id": "text", "name": "text",
              "modules": { "$modules": { "$interfaces": {
                "templates": {
                  "$view": { "$key": "id", "id": "text", "label": "text" },
                  "$mut": { "create({ id: text, label: text })": { "$return": { "id": "text", "label": "text" } } }
                }
              } } }
            },
            "$public": {
              "admin": { "$mut": { "create": "/companies[\"acme\"].modules[\"kit\"].create_template" } }
            }
        } }"#,
    );
    assert!(built.has_code("M-SURFACE"), "expected a surface rejection, got: {}", built.rendered());
    assert!(built.points_at("create_template"));
}

/// §13.12 — a `$ref` whose target is an imported module interface (`#people`)
/// crosses a module boundary, so it MUST decide `$on_delete` at the ref site.
/// Omitting it is rejected (the model previously mis-rejected such a ref as an
/// unresolvable local collection; §13.12 makes the real fault the missing policy).
#[test]
fn cross_boundary_ref_without_on_delete_rejected() {
    let built = build(
        r##"{ "$liasse": 1, "$module": "t.orders@1.0.0",
            "$use": { "people": "t.people/people@1" },
            "$model": { "orders": { "$key": "id", "id": "text", "owner": { "$ref": "#people" } } },
            "$expose": { "orders": { "$view": ".orders { id, owner }" } }
        }"##,
    );
    assert!(built.has_code("M-DELETE"), "expected a cross-boundary $on_delete rejection, got: {}", built.rendered());
    assert!(built.points_at("#people"));
}

/// §13.12 — the same cross-boundary ref loads once it declares `$on_delete`, and
/// a `#handle` target is not rejected as an unresolvable local collection (its
/// key type binds through the composition, a runtime seam).
#[test]
fn cross_boundary_ref_with_on_delete_loads() {
    let built = build(
        r##"{ "$liasse": 1, "$module": "t.orders@1.0.0",
            "$use": { "people": "t.people/people@1" },
            "$model": { "orders": { "$key": "id", "id": "text",
              "owner": { "$ref": "#people", "$on_delete": "restrict" } } },
            "$expose": { "orders": { "$view": ".orders { id, owner }" } }
        }"##,
    );
    built.expect_ok();
}

/// §13.8 — a module-space interface `$mut` contract name carries an explicit
/// parameter prototype; a malformed prototype (an unknown parameter type) is
/// rejected rather than accepted as an opaque boundary contract.
#[test]
fn interface_mut_contract_malformed_prototype_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.host@1.0.0", "$model": {
            "companies": {
              "$key": "id", "id": "text",
              "modules": { "$modules": { "$interfaces": { "t": {
                "$view": { "$key": "id", "id": "text" },
                "$mut": { "create({ id: notatype })": { "$return": "bool" } }
              } } } }
            }
        } }"#,
    );
    assert!(built.has_code("M-MODULE"), "expected an interface contract rejection, got: {}", built.rendered());
    assert!(built.points_at("create({ id: notatype })"));
}

/// §13.8 — an interface `$mut` contract object carries only `$return`; an unknown
/// member (a typo) is rejected.
#[test]
fn interface_mut_contract_unknown_body_member_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.host@1.0.0", "$model": {
            "companies": {
              "$key": "id", "id": "text",
              "modules": { "$modules": { "$interfaces": { "t": {
                "$view": { "$key": "id", "id": "text" },
                "$mut": { "create({ id: text })": { "$reply": "bool" } }
              } } } }
            }
        } }"#,
    );
    assert!(built.has_code("M-MODULE"), "expected an interface contract rejection, got: {}", built.rendered());
    assert!(built.points_at("$reply"));
}

/// §13.8 — `$return` is a response *shape* (scalar type, struct, ref, or
/// row/view); a bare scalar literal is not a shape and is rejected.
#[test]
fn interface_mut_contract_non_shape_return_rejected() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.host@1.0.0", "$model": {
            "companies": {
              "$key": "id", "id": "text",
              "modules": { "$modules": { "$interfaces": { "t": {
                "$view": { "$key": "id", "id": "text" },
                "$mut": { "create({ id: text })": { "$return": 5 } }
              } } } }
            }
        } }"#,
    );
    assert!(built.has_code("M-MODULE"), "expected a `$return` shape rejection, got: {}", built.rendered());
}

/// §13.8 — a well-formed interface `$mut` map loads: a scalar `$return`, a struct
/// `$return`, a `{ $ref: ... }` response, and a response-free (`{}`) contract all
/// pass, exercising each declared response form.
#[test]
fn interface_mut_contract_well_formed_loads() {
    let built = build(
        r#"{ "$liasse": 1, "$app": "t.host@1.0.0", "$model": {
            "companies": {
              "$key": "id", "id": "text",
              "modules": { "$modules": { "$interfaces": { "templates": {
                "$view": { "$key": "id", "id": "text", "label": "text" },
                "$mut": {
                  "disable({ template: text })": { "$return": "bool" },
                  "create({ id: text, label: text })": { "$return": { "id": "text", "label": "text" } },
                  "clone({ id: text })": { "$return": { "$ref": ".templates" } },
                  "remove({ id: text })": {}
                }
              } } } }
            }
        } }"#,
    );
    built.expect_ok();
}
