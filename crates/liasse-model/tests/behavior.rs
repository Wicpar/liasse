//! Mutation, surface, and seed rejections/acceptance (§8, §10, §5/§9).

// Tests are expected to panic on failure (AGENTS.md).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;
use liasse_model::code;

#[test]
fn write_to_computed_value_rejected() {
    // §5.2/§8.5: a mutation may not assign to a read-only computed value.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.forge@1.0.0"
          "$model": {
            "invoices": {
              "$key": "id"
              "id": "text"
              "subtotal": "int"
              "tax": "int"
              "total": "= .subtotal + .tax"
              "$mut": { "forge": ".total = @total" }
            }
          }
        }"#,
    );
    assert!(built.has_code(code::MUTATION));
    assert!(built.points_at(".total"));
    assert!(built.has_hint());
}

#[test]
fn return_not_final_statement_rejected() {
    // §8.10: `return` may appear only as the final statement.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.ret@1.0.0"
          "$model": {
            "tasks": {
              "$key": "id"
              "id": "text"
              "done": "bool = false"
              "$mut": { "bad": ["return .done", ".done = true"] }
            }
          }
        }"#,
    );
    assert!(built.has_code(code::MUTATION));
    assert!(built.has_hint());
}

#[test]
fn assert_condition_must_be_bool() {
    // §8.8: an assert condition is a bool.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.assert@1.0.0"
          "$model": {
            "count": "int"
            "$mut": { "check_it": "assert(.count, 'nope')" }
          }
        }"#,
    );
    assert!(built.has_code(code::MUTATION));
}

#[test]
fn surface_exposes_undeclared_mutation_rejected() {
    // §10.1: a surface mutation reference must name a declared mutation.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.surf@1.0.0"
          "$model": {
            "tasks": { "$key": "id", "id": "text" }
            "$public": {
              "s": { "$mut": { "go": ".does_not_exist" } }
            }
          }
        }"#,
    );
    assert!(built.has_code(code::SURFACE));
    assert!(built.has_hint());
}

#[test]
fn explicit_prototype_declares_parameter() {
    // §8.3: an explicit prototype declares a parameter the body cannot infer.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.proto@1.0.0"
          "$model": {
            "settings": {
              "$key": "id"
              "id": "text"
              "note": "text"
              "$mut": { "set_note({ note: text })": ".note = @note" }
            }
          }
        }"#,
    );
    let model = built.expect_ok();
    let set_note = model
        .mutations()
        .iter()
        .find(|m| m.name.as_str() == "set_note")
        .expect("set_note present");
    let note = set_note
        .params
        .iter()
        .find(|(name, _)| name == "note")
        .expect("prototype parameter present");
    assert_eq!(note.1.describe(), "text");
}

#[test]
fn seed_value_type_mismatch_rejected() {
    // §5/§9: a seed value must conform to the declared field type.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.seed@1.0.0"
          "$model": { "count": "int" }
          "$data": { "count": "not-a-number" }
        }"#,
    );
    assert!(built.has_code(code::SEED));
    assert!(built.has_hint());
}

#[test]
fn seed_value_conforms_accepted() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.seedok@1.0.0"
          "$model": { "count": "int" }
          "$data": { "count": "42" }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn multiple_errors_accumulated() {
    // The builder reports every problem, not just the first.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.multi@1.0.0"
          "$model": {
            "_bad": "text"
            "things": { "$key": "missing", "status": { "$enum": ["a", "a"] } }
          }
        }"#,
    );
    assert!(built.has_code(code::NAME_GRAMMAR));
    assert!(built.has_code(code::KEY));
    assert!(built.has_code(code::ENUM));
}

#[test]
fn param_inferred_from_assignment_target_with_optionality() {
    // §8.3: "CEL typing infers a parameter from every use of `@name`" and
    // "`@name` inherits `.name`'s type and optionality" — the spec's own
    // `"rename": ".name = @name"` example against an optional text field.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.inferassign@1.0.0"
          "$model": {
            "people": {
              "$key": "id"
              "id": "text"
              "name": "text?"
              "$mut": { "rename": ".name = @name" }
            }
          }
        }"#,
    );
    let model = built.expect_ok();
    let rename = model
        .mutations()
        .iter()
        .find(|m| m.name.as_str() == "rename")
        .expect("rename present");
    let name = rename
        .params
        .iter()
        .find(|(name, _)| name == "name")
        .expect("@name inferred");
    // Optionality is inherited, not stripped: the contract type is `text?`.
    assert_eq!(
        name.1.as_scalar(),
        Some(&liasse_value::Type::Optional(Box::new(liasse_value::Type::Text)))
    );
}

#[test]
fn param_inferred_from_collection_key_selector() {
    // §8.3: "`@id` inherits `.tasks.$key`" — the spec's own
    // `"complete": ".tasks[@id].done = true"` example.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.inferkey@1.0.0"
          "$model": {
            "tasks": {
              "$key": "id"
              "id": "text"
              "done": "bool = false"
            }
            "$mut": { "complete": ".tasks[@id].done = true" }
          }
        }"#,
    );
    let model = built.expect_ok();
    let complete = model
        .mutations()
        .iter()
        .find(|m| m.name.as_str() == "complete")
        .expect("complete present");
    let id = complete
        .params
        .iter()
        .find(|(name, _)| name == "id")
        .expect("@id inferred");
    assert_eq!(id.1.as_scalar(), Some(&liasse_value::Type::Text));
}

#[test]
fn uninferable_unprototyped_param_rejected() {
    // §8.3: "All uses of the same parameter MUST infer one compatible type",
    // and "An explicit prototype resolves ambiguity or declares a structure
    // that the body cannot uniquely infer." `return @value` constrains @value
    // to no type, no prototype is declared, so no single contract type exists
    // (the parameter shape "is part of the external surface contract") and the
    // package must not load.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.noinfer@1.0.0"
          "$model": {
            "$mut": { "echo": "return @value" }
          }
        }"#,
    );
    assert!(built.has_code(code::MUTATION));
    // The diagnostic names the parameter at its use...
    assert!(built.points_at("@value"));
    assert!(built
        .expect_err()
        .iter()
        .any(|d| d.message().contains("@value") && d.message().contains("cannot be inferred")));
    // ...and hints at the prototype form that §8.3 provides for this case.
    assert!(built
        .expect_err()
        .iter()
        .any(|d| d.helps().iter().any(|h| h.contains("prototype"))));
}

#[test]
fn param_inferred_from_patch_shorthand_member() {
    // §8.6: the `@name` patch shorthand means `name = @name`, so `@title`
    // inherits the `title` field's type from the patched row (§8.3).
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.patchshort@1.0.0"
          "$model": {
            "tasks": {
              "$key": "id"
              "id": "text"
              "title": "text"
              "note": "text?"
            }
            "$mut": { "edit": ".tasks[@id] { @title, -note }" }
          }
        }"#,
    );
    let model = built.expect_ok();
    assert_eq!(param_type(model, "edit", "title"), &liasse_value::Type::Text);
}

#[test]
fn param_inferred_through_nested_struct_literal() {
    // §5.3/§8.3: a nested struct-literal insert value carries its own members;
    // `@line1` inherits the `address.line1` field type, not the receiver's.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.neststruct@1.0.0"
          "$model": {
            "orders": {
              "$key": "id"
              "id": "text"
              "address": { "line1": "text", "city": "text", "zip": "int" }
            }
            "$mut": {
              "add": "row = .orders + { id: @id, address: { line1: @line1, zip: @zip } }"
            }
          }
        }"#,
    );
    let model = built.expect_ok();
    assert_eq!(param_type(model, "add", "line1"), &liasse_value::Type::Text);
    // A deeper member keeps its own type, proving the walk descends by field.
    assert_eq!(param_type(model, "add", "zip"), &liasse_value::Type::Int);
}

#[test]
fn in_program_no_parameter_call_accepted() {
    // §8.11/§8.5: an in-program mutation call (`bump()`, `.bump({})`) is not an
    // expression function; the phase accepts it structurally rather than
    // rejecting it as an unknown function.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.inprog@1.0.0"
          "$model": {
            "counter": { "n": "int = 0" }
            "$mut": {
              "bump": ".counter.n = .counter.n + 1"
              "apply": ["bump()", "return .counter { n }"]
              "apply_empty": [".bump({})", "return .counter { n }"]
            }
          }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn param_used_only_as_call_argument_is_deferred() {
    // §8.3/§16.4/§8.11: a parameter whose only use is a call argument inherits
    // its type from the callee's declared signature. The CORE model does not
    // resolve host namespaces, so such a parameter is deferred, not rejected —
    // the package loads even though the model cannot pin `@n` here.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.callarg@1.0.0"
          "$requires": { "u": "test.util@1" }
          "$model": {
            "results": { "$key": "id", "id": "text", "v": "int" }
            "$mut": {
              "compute": ["r = .results + { id: @id, v: u.f(@n) }", "return r { id, v }"]
            }
          }
        }"#,
    );
    let model = built.expect_ok();
    // `@id` is still inferred from the insert; `@n` is simply absent (deferred),
    // never a rejection.
    assert_eq!(param_type(model, "compute", "id"), &liasse_value::Type::Text);
    let compute = model.mutations().iter().find(|m| m.name.as_str() == "compute").expect("compute");
    assert!(!compute.params.iter().any(|(name, _)| name == "n"), "@n is deferred, not pinned");
}

#[test]
fn param_only_in_return_still_rejected() {
    // §8.3 guardrail: deferring call-argument parameters must not defer a
    // genuinely uninferable one. `return @value` is not a call argument, so it
    // still fails to infer a single type and rejects.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.retonly@1.0.0"
          "$model": { "$mut": { "echo": "return @value" } }
        }"#,
    );
    assert!(built.has_code(code::MUTATION));
}

/// The type a mutation infers for one parameter, panicking if it is absent.
fn param_type<'a>(model: &'a liasse_model::Model, mutation: &str, param: &str) -> &'a liasse_value::Type {
    model
        .mutations()
        .iter()
        .find(|m| m.name.as_str() == mutation)
        .unwrap_or_else(|| panic!("mutation `{mutation}` present"))
        .params
        .iter()
        .find(|(name, _)| name == param)
        .unwrap_or_else(|| panic!("parameter `@{param}` inferred"))
        .1
        .as_scalar()
        .unwrap_or_else(|| panic!("parameter `@{param}` is scalar"))
}

#[test]
fn surface_view_root_projection_accepted() {
    // §7.1/§10.1/§12.2: a surface `$view` projecting the root singleton
    // (`. { a, b }`) yields a single row, delivered as one object — it is a
    // valid external read result and must not be rejected as "not a stream".
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.rootproj@1.0.0"
          "$model": {
            "a": "int"
            "b": "int"
            "sum": "= .a + .b"
            "$public": { "v": { "$view": ". { sum }" } }
          }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn model_view_struct_projection_accepted() {
    // §7.1/§5.3: a named `$view` projecting a static struct (`.invoice { ... }`)
    // types as a single row; §12.2 delivers it as one object. Accepted.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.structview@1.0.0"
          "$model": {
            "invoice": { "subtotal": "int", "tax": "int", "total": "= .subtotal + .tax" }
            "view_invoice": { "$view": ".invoice { subtotal, tax, total }" }
            "$public": { "invoice": { "$view": ".view_invoice" } }
          }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn surface_view_scalar_aggregate_accepted() {
    // §7.5/§12.2: a surface `$view` yielding a scalar aggregate (`count(...)`)
    // is a valid read result delivered as one value; it must load.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.scalarview@1.0.0"
          "$model": {
            "items": { "$key": "id", "id": "text" }
            "$public": { "n": { "$view": "count(.items)" } }
          }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn param_inferred_from_keyed_patch_block_assign() {
    // §8.3 ("infers a parameter from every use of `@name`") + §8.6: in a keyed
    // patch `.tasks[@id] { title = @title }`, `@id` inherits `.tasks.$key` and
    // `@title` inherits `.tasks.title` — the assignment (`field = value`) patch
    // member form, not only the projection (`field: value`) form.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.blockassign@1.0.0"
          "$model": {
            "tasks": { "$key": "id", "id": "text", "title": "text" }
            "$mut": { "retitle": ".tasks[@id] { title = @title }" }
          }
        }"#,
    );
    let model = built.expect_ok();
    assert_eq!(param_type(model, "retitle", "id"), &liasse_value::Type::Text);
    assert_eq!(param_type(model, "retitle", "title"), &liasse_value::Type::Text);
}

#[test]
fn param_inferred_from_comparison_and_arithmetic_operand() {
    // §8.3: a scalar comparison or arithmetic relates its operands to one type,
    // so `@amount` in `assert(.balance >= @amount)` and in `.balance - @amount`
    // inherits `.balance`'s `int` type — no prototype required.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.operand@1.0.0"
          "$model": {
            "accounts": {
              "$key": "id", "id": "text", "balance": "int"
              "$mut": {
                "withdraw": [
                  "assert(.balance >= @amount, 'Insufficient funds')"
                  ".balance = .balance - @amount"
                ]
              }
            }
          }
        }"#,
    );
    let model = built.expect_ok();
    assert_eq!(param_type(model, "withdraw", "amount"), &liasse_value::Type::Int);
}

#[test]
fn param_inferred_from_filter_bind_key_comparison() {
    // §8.3 + §6.4: inside a filtered selector `[:x | x.id == @a]` the binding
    // `x` is a row of `.things`, so `@a` (compared to `x.id`) inherits the
    // `id` field's `text` type across the row binding.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.filterbind@1.0.0"
          "$model": {
            "things": { "$key": "id", "id": "text" }
            "$mut": { "purge": "-.things[:x | x.id == @a || x.id == @b]" }
          }
        }"#,
    );
    let model = built.expect_ok();
    assert_eq!(param_type(model, "purge", "a"), &liasse_value::Type::Text);
    assert_eq!(param_type(model, "purge", "b"), &liasse_value::Type::Text);
}

#[test]
fn collection_sort_declaration_accepted() {
    // §7.3: "Collections and views MAY declare `$sort`." A collection-level
    // `$sort` of comparison keys (with a leading `-` for descending) loads.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.sort@1.0.0"
          "$model": {
            "tasks": {
              "$key": "id", "id": "text", "title": "text", "created_at": "timestamp"
              "$sort": ["title", "-created_at"]
            }
          }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn structured_sort_key_accepted() {
    // §7.3: the structured `{ $by, $dir }` sort-key form is accepted.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.sort2@1.0.0"
          "$model": {
            "tasks": {
              "$key": "id", "id": "text", "name": "text"
              "$sort": [ { "$by": "name", "$dir": "desc" } ]
            }
          }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn malformed_sort_direction_rejected() {
    // §7.3: `$dir` is `asc` or `desc`; any other value is rejected.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.sort3@1.0.0"
          "$model": {
            "tasks": {
              "$key": "id", "id": "text", "name": "text"
              "$sort": [ { "$by": "name", "$dir": "sideways" } ]
            }
          }
        }"#,
    );
    assert!(built.has_code("M-SORT"));
}

#[test]
fn nested_role_mutation_reference_scoped_to_containing_row() {
    // §10.3: "Roles MAY be nested on application rows. Their location defines
    // scope." A role nested on `companies` exposes `$mut: { rename: ".rename" }`
    // whose `.` is the containing companies row, so `.rename` resolves to the
    // mutation declared on `companies` — not the model root. It must load.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.scoped@1.0.0"
          "$model": {
            "accounts": { "$key": "id", "id": "text" }
            "companies": {
              "$key": "id", "id": "text", "name": "text"
              "members": { "$key": "account", "account": { "$ref": "/accounts" }, "admin": "bool = false" }
              "$mut": { "rename": [".name = @name", "return . { id, name }"] }
              "$roles": {
                "admin": {
                  "$auth": "token"
                  "$members": ".members[:m | m.admin].account"
                  "company": {
                    "$view": ". { id, name }"
                    "$mut": { "rename": ".rename" }
                  }
                }
              }
            }
            "$auth": { "token": { "$credential": "text", "$verify": "$credential", "$actor": "/accounts[$proof]" } }
          }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn root_surface_mutation_reference_still_resolves() {
    // Regression: a root `$public` surface's `.` remains the model root, so a
    // root-level mutation reference resolves against path [] (§10.1).
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.rootsurf@1.0.0"
          "$model": {
            "tasks": { "$key": "id", "id": "text", "title": "text" }
            "$mut": { "add": ".tasks + { title: @title }" }
            "$public": { "tasks": { "$mut": { "add": ".add" } } }
          }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn param_inferred_from_composite_key_object_selector_by_name() {
    // §6.3 + Annex A.9: a composite-key selector names each key component in an
    // object operand, and the binding is by component name, not member position.
    // The members are written in the reverse of `$key` order, so each parameter
    // must still inherit its own component's type (`country`/`code` are both
    // `text` here, but a positional binder would still have to resolve them via
    // the struct key, which this asserts by keeping the reversed order valid).
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.compkey@1.0.0"
          "$model": {
            "vat_rates": {
              "$key": ["country", "code"]
              "country": "text", "code": "int", "rate": "int"
            }
            "$mut": { "read": "return .vat_rates[{ code: @code, country: @country }] { rate }" }
          }
        }"#,
    );
    let model = built.expect_ok();
    // `code` is `int` and `country` is `text`: a positional binder would swap
    // them (reversed member order), so matching by name is what makes this hold.
    assert_eq!(param_type(model, "read", "code"), &liasse_value::Type::Int);
    assert_eq!(param_type(model, "read", "country"), &liasse_value::Type::Text);
}

#[test]
fn param_inferred_from_meter_accessor_time_context() {
    // §15.6 + Annex §15 grammar (`$time?: timestamp-expression`): a parameter in
    // the reserved structural `$time` member of a hypothetical meter-accessor
    // context inherits `timestamp`, even though the accessor call itself is an
    // opaque runtime seam.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.hypo@1.0.0"
          "$model": {
            "users": {
              "$key": "id", "id": "text"
              "topups": { "$key": "id", "id": "text", "amount": "decimal" }
              "$limits": { "credits": { "$sources": { "topup": ".topups { $quantity: .amount }" } } }
              "$mut": { "probe": "return .credits.balance({ $time: @at })" }
            }
          }
        }"#,
    );
    let model = built.expect_ok();
    assert_eq!(param_type(model, "probe", "at"), &liasse_value::Type::timestamp());
}

#[test]
fn param_inferred_through_all_temporal_selector_filter_bind() {
    // §14.2 + §8.3: `.things.$all` is a temporal selector that preserves the
    // bucketed base view's row shape, so a filtered bind over it resolves the
    // same rows and `@k` (compared to the bound row's key field) is inferred.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.allbind@1.0.0"
          "$model": {
            "things": { "$key": "id", "$bucket": ".ends_at", "id": "text", "ends_at": "timestamp" }
            "$mut": { "purge": "-.things.$all[:t | t.id == @k]" }
          }
        }"#,
    );
    let model = built.expect_ok();
    assert_eq!(param_type(model, "purge", "k"), &liasse_value::Type::Text);
}

#[test]
fn view_expression_equals_marker_stripped() {
    // §4.2: a `$view` value is an expression; an optional leading `=` marker is
    // accepted and stripped, so a scalar/aggregate view loads like a bare one.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.viewmarker@1.0.0"
          "$model": {
            "docs": { "$key": "id", "id": "text" }
            "count": { "$view": "= size(.docs)" }
          }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn like_positional_recursion_inline_shape_accepted() {
    // §5.8: `$like: "^"` adopts the immediately containing shape even when that
    // shape is an inline model-tree collection (not a named `$types` shape).
    // `categories.children` therefore becomes a keyed collection of the same
    // contract, so a view descending `.categories[:c].children[:k]` type-checks.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.likerec@1.0.0"
          "$model": {
            "categories": {
              "$key": "id", "id": "text", "name": "text"
              "children": { "$like": "^" }
            }
            "descendants": {
              "$view": ".categories[:c].children[:k] { parent: c.id, child: k.id, name: k.name }"
            }
          }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn like_positional_recursion_bad_target_rejected() {
    // §5.8: `$like` names a lexical shape by `^` depth; a non-`^` value is not a
    // positional-recursion reference and is rejected with a hint.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.likebad@1.0.0"
          "$model": {
            "categories": {
              "$key": "id", "id": "text"
              "children": { "$like": "categories" }
            }
          }
        }"#,
    );
    assert!(built.has_code("M-TYPE"));
    assert!(built.has_hint());
}

#[test]
fn surface_params_expanded_field_form_accepted() {
    // §10.1/§12.1: a `$params` entry may be an expanded field declaration
    // carrying `$type` alongside the request-scoped `$normalize`/`$check`, not
    // only a bare type string. The `$type` supplies the parameter's type.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.paramnorm@1.0.0"
          "$model": {
            "tasks": { "$key": "id", "id": "uuid = uuid()", "title": "text" }
            "$mut": { "add": ["t = .tasks + { title: @title }", "return t { id, title }"] }
            "index": { "$view": ".tasks { id, title }" }
            "$public": {
              "tasks": {
                "$params": {
                  "title": {
                    "$type": "text"
                    "$normalize": "string.trim(.)"
                    "$check": ["size(.) > 0", "A title is required"]
                  }
                }
                "$view": ".index"
                "$mut": { "add": ".add({ title: @title })" }
              }
            }
          }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn surface_params_missing_type_rejected() {
    // §10.1: an expanded `$params` field declaration with no `$type` cannot type
    // the parameter and is rejected with a hint.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.paramnotype@1.0.0"
          "$model": {
            "tasks": { "$key": "id", "id": "text" }
            "index": { "$view": ".tasks { id }" }
            "$public": {
              "tasks": {
                "$params": { "title": { "$normalize": "string.trim(.)" } }
                "$view": ".index"
              }
            }
          }
        }"#,
    );
    assert!(built.has_code(code::SURFACE));
    assert!(built.has_hint());
}

/// A role package whose `admin.company` surface carries the given `$recursive`
/// body (the members between the block's braces), over a self-similar
/// `subcompanies` tree.
fn recursive_package(recursive_body: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1
          "$app": "t.rec@1.0.0"
          "$model": {{
            "accounts": {{ "$key": "id", "id": "text" }}
            "companies": {{
              "$key": "id"
              "id": "text"
              "name": "text"
              "plan": "text = 'active'"
              "subcompanies": {{ "$like": "^" }}
              "members": {{
                "$key": "account"
                "account": {{ "$ref": "/accounts" }}
                "admin": "bool = false"
              }}
              "$roles": {{
                "admin": {{
                  "$auth": "token"
                  "$members": ".members[:m | m.admin].account"
                  "company": {{
                    "$view": ". {{ id, name, plan }}"
                    "$recursive": {{ {recursive_body} }}
                  }}
                }}
              }}
            }}
            "$auth": {{
              "token": {{ "$credential": "text", "$verify": "$credential", "$actor": "/accounts[$proof]" }}
            }}
          }}
        }}"#
    )
}

#[test]
fn recursive_coverage_with_bool_predicate_accepted() {
    // §10.5: a role MAY propagate a surface through a checked descendant
    // relation; a `$where` predicate over the bound candidate that is `bool`
    // loads.
    let built = build(&recursive_package(
        r#""$field": "subcompanies", "$through": ".subcompanies", "$bind": "child", "$where": "child.plan != 'closed'""#,
    ));
    built.expect_ok();
}

#[test]
fn recursive_coverage_non_bool_predicate_rejected() {
    // §10.5: "The checker verifies ... predicate types." A `$where` that yields
    // text, not bool, is rejected.
    let built = build(&recursive_package(
        r#""$field": "subcompanies", "$through": ".subcompanies", "$bind": "child", "$where": "child.name""#,
    ));
    assert!(built.has_code(code::SURFACE));
    assert!(built.expect_err().iter().any(|d| d.message().contains("bool")));
}

#[test]
fn recursive_coverage_missing_required_member_rejected() {
    // §10.5: "`$field`, `$through`, and `$bind` are required." A block missing
    // `$bind` cannot be checked and is rejected with a hint.
    let built = build(&recursive_package(
        r#""$field": "subcompanies", "$through": ".subcompanies""#,
    ));
    assert!(built.has_code(code::SURFACE));
    assert!(built.has_hint());
}

#[test]
fn recursive_coverage_non_stream_through_rejected() {
    // §10.5: `$through` "yields strict descendants" — it must resolve to a row
    // stream. A scalar-field traversal is not a descendant relation.
    let built = build(&recursive_package(
        r#""$field": "name", "$through": ".name", "$bind": "child""#,
    ));
    assert!(built.has_code(code::SURFACE));
}
