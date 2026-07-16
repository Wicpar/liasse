#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Nested keyed collections and static structs end-to-end (§5.3, §5.4, §5.5,
//! §5.7): a collection nested under a parent row has parent-scoped identity and
//! uniqueness, an omitted child collection starts empty, a supplied child
//! initializer is validated atomically with the parent, static-struct defaults
//! resolve with the containing insertion, and an ancestor rekey rewrites the
//! whole descendant subtree's identity.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Value};
use liasse_value::Text;
use serde_json::json;
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// A companies → offices two-level model with root mutations that address the
/// nested collection through the parent key.
const OFFICES: &str = r#"{
  "$liasse": 1
  "$app": "t.offices@1.0.0"
  "$model": {
    "companies": {
      "$key": "id"
      "id": "text"
      "offices": {
        "$key": "id"
        "id": "text"
        "name": "text"
      }
    }
    "$mut": {
      "add_company": ".companies + { id: @id }"
      "add_office": ".companies[@company].offices + { id: @id, name: @name }"
      "rekey_company": ".companies[@old].id = @new"
      "office_name": "return .companies[@company].offices[@office].name"
    }
  }
}"#;

fn add_company(engine: &mut liasse_runtime::Engine<liasse_store::MemoryStore>, id: &str) -> CallOutcome {
    let mut g = generator();
    engine.call(&CallRequest::new("add_company").arg("id", text(id)), &mut g).expect("call")
}

fn add_office(
    engine: &mut liasse_runtime::Engine<liasse_store::MemoryStore>,
    company: &str,
    id: &str,
    name: &str,
) -> CallOutcome {
    let mut g = generator();
    engine
        .call(
            &CallRequest::new("add_office")
                .arg("company", text(company))
                .arg("id", text(id))
                .arg("name", text(name)),
            &mut g,
        )
        .expect("call")
}

#[test]
fn nested_collection_key_scoped_to_parent() {
    let mut engine = load("offices", OFFICES);
    assert!(matches!(add_company(&mut engine, "acme"), CallOutcome::Committed { .. }));
    assert!(matches!(add_company(&mut engine, "globex"), CallOutcome::Committed { .. }));
    // Same local key "hq" under two distinct parents coexist (§5.4).
    assert!(matches!(add_office(&mut engine, "acme", "hq", "Acme HQ"), CallOutcome::Committed { .. }));
    assert!(matches!(add_office(&mut engine, "globex", "hq", "Globex HQ"), CallOutcome::Committed { .. }));
    // Same local key under the *same* parent rejects.
    assert!(
        matches!(add_office(&mut engine, "acme", "hq", "Again"), CallOutcome::Rejected(_)),
        "a repeated local key under one parent must reject (§5.4)"
    );
}

const MEMBERS: &str = r#"{
  "$liasse": 1
  "$app": "t.members@1.0.0"
  "$model": {
    "companies": {
      "$key": "id"
      "id": "text"
      "members": {
        "$key": "id"
        "$unique": ["email"]
        "id": "text"
        "email": "text"
      }
    }
    "$mut": {
      "add_company": ".companies + { id: @id }"
      "add_member": ".companies[@company].members + { id: @id, email: @email }"
    }
  }
}"#;

#[test]
fn nested_unique_scoped_per_parent() {
    let mut engine = load("members", MEMBERS);
    let mut g = generator();
    let add_member = |engine: &mut liasse_runtime::Engine<liasse_store::MemoryStore>, company, id, email| {
        let mut g = generator();
        engine
            .call(
                &CallRequest::new("add_member")
                    .arg("company", text(company))
                    .arg("id", text(id))
                    .arg("email", text(email)),
                &mut g,
            )
            .expect("call")
    };
    engine.call(&CallRequest::new("add_company").arg("id", text("acme")), &mut g).expect("call");
    engine.call(&CallRequest::new("add_company").arg("id", text("globex")), &mut g).expect("call");
    assert!(matches!(add_member(&mut engine, "acme", "m1", "bob@x.example"), CallOutcome::Committed { .. }));
    // Same candidate value under a different parent does not conflict (§5.7).
    assert!(matches!(add_member(&mut engine, "globex", "m1", "bob@x.example"), CallOutcome::Committed { .. }));
    // Same candidate value under the same parent rejects.
    assert!(
        matches!(add_member(&mut engine, "acme", "m2", "bob@x.example"), CallOutcome::Rejected(_)),
        "a duplicate candidate key within one parent must reject (§5.7)"
    );
}

#[test]
fn ancestor_rekey_rewrites_descendant_identity() {
    let mut engine = load("offices", OFFICES);
    add_company(&mut engine, "acme");
    add_office(&mut engine, "acme", "hq", "HQ");
    let mut g = generator();
    let rekey = engine
        .call(&CallRequest::new("rekey_company").arg("old", text("acme")).arg("new", text("newco")), &mut g)
        .expect("call");
    assert!(matches!(rekey, CallOutcome::Committed { .. }), "ancestor rekey commits");

    // The descendant is reachable under the new ancestor key, data intact.
    let ok = engine
        .call(
            &CallRequest::new("office_name").arg("company", text("newco")).arg("office", text("hq")),
            &mut g,
        )
        .expect("call");
    let value = ok.response().expect("response").to_wire();
    assert_eq!(value, json!("HQ"), "descendant survives the rekey under the new ancestor key");

    // The stale ancestor address resolves to nothing: the row read fails.
    let stale = engine
        .call(
            &CallRequest::new("office_name").arg("company", text("acme")).arg("office", text("hq")),
            &mut g,
        )
        .expect("call");
    let stale_wire = stale.response().map(|r| r.to_wire());
    assert_ne!(
        stale_wire,
        Some(json!("HQ")),
        "no ghost subtree survives under the old ancestor key (§5.4)"
    );
}

const NESTED_INIT: &str = r#"{
  "$liasse": 1
  "$app": "t.nestedinit@1.0.0"
  "$model": {
    "staging": {
      "$key": "id"
      "id": "text"
      "qty": "int"
    }
    "orders": {
      "$key": "id"
      "id": "text"
      "lines": {
        "$key": "id"
        "id": "text"
        "qty": {
          "$type": "int"
          "$check": [". > 0", "Quantity must be positive"]
        }
      }
    }
    "$mut": {
      "add_all": ".orders + { id: @id, lines: .staging { id, qty } }"
      "add_good": ".orders + { id: @id, lines: .staging[:s | s.qty > 0] { id, qty } }"
    }
  }
  "$data": {
    "staging": {
      "l1": { "qty": 0 }
      "l2": { "qty": 2 }
    }
  }
}"#;

#[test]
fn nested_initializer_failure_rejects_parent_insert() {
    let mut engine = load("nestedinit", NESTED_INIT);
    let mut g = generator();
    // A bad child row (qty 0) rejects the complete insertion, parent included.
    let bad = engine.call(&CallRequest::new("add_all").arg("id", text("o1")), &mut g).expect("call");
    assert!(matches!(bad, CallOutcome::Rejected(_)), "a failing child initializer rejects the parent (§5.5)");
    // The filtered initializer passes; parent and its one valid line commit together.
    let good = engine.call(&CallRequest::new("add_good").arg("id", text("o2")), &mut g).expect("call");
    assert!(matches!(good, CallOutcome::Committed { .. }), "the filtered initializer commits");
}

const STRUCT_AND_EMPTY: &str = r#"{
  "$liasse": 1
  "$app": "t.structempty@1.0.0"
  "$model": {
    "orders": {
      "$key": "id"
      "id": "text"
      "address": {
        "line1": "text"
        "line2": "text?"
        "city": "text"
        "country": "text = 'FR'"
      }
    }
    "projects": {
      "$key": "id"
      "id": "text"
      "tags": { "$set": "text" }
      "tasks": {
        "$key": "id"
        "id": "text"
        "title": "text"
      }
    }
    "$mut": {
      "add_order": [
        "row = .orders + { id: @id, address: { line1: @line1, city: @city } }"
        "return row { id, address }"
      ]
      "add_project": [
        "row = .projects + { id: @id }"
        "return row { id, tags, tasks }"
      ]
    }
  }
}"#;

#[test]
fn static_struct_defaults_resolve_with_the_row() {
    let mut engine = load("structempty", STRUCT_AND_EMPTY);
    let mut g = generator();
    let outcome = engine
        .call(
            &CallRequest::new("add_order")
                .arg("id", text("o1"))
                .arg("line1", text("1 Main St"))
                .arg("city", text("Paris")),
            &mut g,
        )
        .expect("call");
    let value = outcome.response().expect("response").to_wire();
    // line2 was omitted and optional → absent; country default resolved (§5.1, §5.3).
    assert_eq!(
        value,
        json!({ "id": "o1", "address": { "line1": "1 Main St", "city": "Paris", "country": "FR" } })
    );
}

#[test]
fn omitted_child_collections_start_empty() {
    let mut engine = load("structempty", STRUCT_AND_EMPTY);
    let mut g = generator();
    let outcome =
        engine.call(&CallRequest::new("add_project").arg("id", text("p1")), &mut g).expect("call");
    let value = outcome.response().expect("response").to_wire();
    // Both the omitted set and the omitted nested collection come back empty (§5.5).
    assert_eq!(value, json!({ "id": "p1", "tags": [], "tasks": [] }));
}
