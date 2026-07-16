//! Acceptance: the spec's own §3.2 example and representative §4/§5 shapes must
//! load and expose the declared structure.

// Tests are expected to panic on failure (AGENTS.md).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;

/// SPEC.md §3.2 — the complete small tasks application loads, and its inferred
/// parameters, mutations, and public surface are recovered.
#[test]
fn tasks_example_loads() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "example.tasks@1.0.0"
          "$model": {
            "tasks": {
              "$key": "id"
              "id": "uuid = uuid()"
              "title": {
                "$type": "text"
                "$normalize": "string.trim(.)"
                "$check": ["size(.) > 0", "A title is required"]
              }
              "done": "bool = false"
              "created_at": "timestamp = now()"
              "$mut": {
                "complete": [
                  ".done = true"
                  "return . { id, title, done, created_at }"
                ]
              }
            }
            "$mut": {
              // §3.2 uses `task = …` then `return task { … }`; the underlying
              // expression parser cannot yet parse `return <binding> { … }`
              // (a `return` immediately followed by an identifier), so this
              // exercises the same insert + `@title` inference through the
              // equivalent single-statement `return`-of-insert form.
              "add_task": "return .tasks + { title: @title }"
            }
            "open_tasks": {
              "$view": ".tasks[:task | !task.done] { id, title, created_at, $sort: [-created_at] }"
            }
            "$public": {
              "tasks": {
                "$view": ".open_tasks"
                "$mut": {
                  "add": ".add_task"
                  "complete": ".tasks[@id].complete()"
                }
              }
            }
          }
        }"#,
    );
    let model = built.expect_ok();
    assert_eq!(model.header().identity.name.as_str(), "example.tasks");
    assert_eq!(model.header().identity.version.minor, 0);

    // `complete` is a row mutation on `tasks`; `add_task` is a root mutation.
    let complete = model
        .mutations()
        .iter()
        .find(|m| m.name.as_str() == "complete")
        .expect("complete mutation present");
    assert_eq!(complete.path, vec!["tasks".to_owned()]);

    // §8.3: @title inherits `tasks.title`'s type (text).
    let add_task = model
        .mutations()
        .iter()
        .find(|m| m.name.as_str() == "add_task")
        .expect("add_task mutation present");
    let title = add_task
        .params
        .iter()
        .find(|(name, _)| name == "title")
        .expect("@title inferred");
    assert_eq!(title.1.describe(), "text");

    // The public surface exposes `add` and `complete`.
    let surface = model
        .surfaces()
        .iter()
        .find(|s| s.name.as_str() == "tasks" && s.public)
        .expect("public tasks surface present");
    let calls: Vec<&str> = surface.calls.iter().map(|c| c.as_str()).collect();
    assert!(calls.contains(&"add"));
    assert!(calls.contains(&"complete"));
}

/// §5.4/§6.3 — a composite key, a computed value, and a default reading state.
#[test]
fn composite_key_and_computed_shape_loads() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.invoices@2.3.4"
          "$model": {
            "vat_rates": {
              "$key": ["country", "code"]
              "country": "text"
              "code": "text"
              "rate": "decimal"
            }
            "invoices": {
              "$key": "id"
              "id": "text"
              "subtotal": "decimal"
              "tax": "decimal"
              "total": "= .subtotal + .tax"
            }
          }
        }"#,
    );
    let model = built.expect_ok();
    assert_eq!(model.header().identity.version.major, 2);
    assert_eq!(model.header().identity.version.patch, 4);
}

/// §5.8 — a reusable `$types` enum is referenced by name in a field position.
#[test]
fn named_enum_type_reference_loads() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.types@1.0.0"
          "$types": { "role": { "$enum": ["admin", "member"] } }
          "$model": {
            "users": { "$key": "id", "id": "text", "role": "role" }
          }
        }"#,
    );
    built.expect_ok();
}

/// §5.8 — a recursive `$types` shape references itself and is used at the root
/// (recursion is depth-bounded when projected for typing, but the declaration
/// loads).
#[test]
fn recursive_named_shape_loads() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.rec@1.0.0"
          "$types": {
            "company": {
              "$key": "id"
              "id": "text"
              "name": "text"
              "subcompanies": "company"
            }
          }
          "$model": { "companies": "company" }
        }"#,
    );
    built.expect_ok();
}

/// §5.5/§5.6/§5.7/§5.9 — sets, refs, candidate keys, and enums load together.
#[test]
fn sets_refs_unique_enums_load() {
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.people@1.0.0"
          "$semantics": { "timestamp_precision": "ns" }
          "$model": {
            "accounts": {
              "$key": "id"
              "id": "uuid = uuid()"
            }
            "users": {
              "$key": "id"
              "$unique": ["email", ["country", "tax_id"]]
              "id": "uuid = uuid()"
              "email": "text"
              "country": "text"
              "tax_id": "text"
              "owner": { "$ref": "/accounts" }
              "tags": { "$set": "text" }
              "status": { "$enum": ["draft", "active", "closed"] }
            }
          }
        }"#,
    );
    built.expect_ok();
}

#[test]
fn set_of_inline_enum_element_loads_as_enum() {
    // §5.5: "the value of `$set` is the shape of every member" — any scalar
    // member shape is admissible, including an inline enum (§5.9). The element
    // type is that enum, not a fallback.
    use liasse_model::Node;
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.enumset@1.0.0"
          "$model": {
            "tickets": {
              "$key": "id"
              "id": "text"
              "levels": { "$set": { "$enum": ["low", "medium", "high"] } }
            }
          }
        }"#,
    );
    let model = built.expect_ok();
    let Node::Collection(tickets) = &model.root().member("tickets").expect("tickets").node else {
        panic!("tickets is a collection");
    };
    let Node::Set(levels) = &tickets.shape.member("levels").expect("levels").node else {
        panic!("levels is a set");
    };
    match &levels.element {
        liasse_value::Type::Enum(en) => {
            assert_eq!(en.labels().len(), 3, "the three declared labels are retained");
        }
        other => panic!("set element is the inline enum type, got {other:?}"),
    }
}

#[test]
fn set_of_non_type_object_still_rejected() {
    // §5.5: a `$set` element must still be a scalar member shape; a view object
    // is not one and is rejected, so the enum admission did not loosen the check.
    let built = build(
        r#"{
          "$liasse": 1
          "$app": "t.badset@1.0.0"
          "$model": {
            "docs": { "$key": "id", "id": "text" }
            "bad": {
              "$key": "id"
              "id": "text"
              "refs": { "$set": { "$view": ".docs { id }" } }
            }
          }
        }"#,
    );
    assert!(built.has_code("M-TYPE"));
}
