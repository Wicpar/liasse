#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §20.1/§22.1: a package update carries NESTED keyed-collection rows (§5.4)
//! exactly as it carries top-level ones — at every depth, and for a collection
//! adopted through a §5.8 `$types` name as much as for a directly-declared one.
//!
//! The §20.1 compatible same-identity copy is defined over committed rows, not
//! over top-level committed rows: a child row is real state living under its
//! parent (§5.4/§5.5), so an update that dropped it would destroy live data while
//! reporting success. These tests pin the copy at depth 2 and depth 3, the added
//! field's insertion default on a nested row (§5.1), a nested `$from`/`$as`
//! transform (§20.1), and the §13.13 `$bundle` three-way merge reaching a nested
//! bundled row.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, UpdateRelation, Value};
use liasse_store::MemoryStore;
use liasse_value::{Integer, Text};
use support::{generator, load};

/// A three-level model — companies → offices → desks — whose rows are created by
/// ordinary mutations, so every nested row is committed state rather than seed.
const DESKS_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.nestmig@1.0.0"
  "$model": {
    "companies": {
      "$key": "id"
      "id": "text"
      "name": "text"
      "offices": {
        "$key": "id"
        "id": "text"
        "city": "text"
        "desks": { "$key": "id", "id": "text", "label": "text" }
      }
    }
    "all_offices": { "$view": ".companies[:c].offices[:o] { company: c.id, office: o.id, city: o.city }" }
    "all_desks": {
      "$view": ".companies[:c].offices[:o].desks[:d] { office: o.id, desk: d.id, label: d.label }"
    }
    "$mut": {
      "add_company": ".companies + { id: @id, name: @name }"
      "add_office": ".companies[@company].offices + { id: @id, city: @city }"
      "add_desk": ".companies[@company].offices[@office].desks + { id: @id, label: @label }"
    }
  }
}"#;

/// A compatible minor release adding a defaulted field at EVERY nesting level.
const DESKS_V1_1: &str = r#"{
  "$liasse": 1
  "$app": "t.nestmig@1.1.0"
  "$model": {
    "companies": {
      "$key": "id"
      "id": "text"
      "name": "text"
      "offices": {
        "$key": "id"
        "id": "text"
        "city": "text"
        "floor": "int = 7"
        "desks": { "$key": "id", "id": "text", "label": "text", "seats": "int = 1" }
      }
    }
    "all_offices": {
      "$view": ".companies[:c].offices[:o] { company: c.id, office: o.id, city: o.city, floor: o.floor }"
    }
    "all_desks": {
      "$view": ".companies[:c].offices[:o].desks[:d] { office: o.id, desk: d.id, label: d.label, seats: d.seats }"
    }
    "$mut": {
      "add_company": ".companies + { id: @id, name: @name }"
      "add_office": ".companies[@company].offices + { id: @id, city: @city }"
      "add_desk": ".companies[@company].offices[@office].desks + { id: @id, label: @label }"
    }
  }
}"#;

/// A major release renaming the depth-3 `label` through a nested `$from`/`$as`
/// mapping — the nested analogue of a top-level field rename (§20.1).
const DESKS_V2: &str = r#"{
  "$liasse": 1
  "$app": "t.nestmig@2.0.0"
  "$model": {
    "companies": {
      "$key": "id"
      "id": "text"
      "name": "text"
      "offices": {
        "$key": "id"
        "id": "text"
        "city": "text"
        "desks": {
          "$key": "id"
          "id": "text"
          "tag": { "$type": "text", "$from": "label", "$as": "string.upper(.)" }
        }
      }
    }
    "all_desks": { "$view": ".companies[:c].offices[:o].desks[:d] { office: o.id, desk: d.id, tag: d.tag }" }
    "$mut": {
      "add_company": ".companies + { id: @id, name: @name }"
      "add_office": ".companies[@company].offices + { id: @id, city: @city }"
    }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

fn call(engine: &mut Engine<MemoryStore>, request: CallRequest) {
    let mut generator = generator();
    let outcome = engine.call(&request, &mut generator).expect("call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the mutation must commit");
}

/// An instance holding one company, two offices, and three desks across them.
fn populated(instance: &str, definition: &str) -> Engine<MemoryStore> {
    let mut engine = load(instance, definition);
    call(&mut engine, CallRequest::new("add_company").arg("id", text("acme")).arg("name", text("Acme")));
    call(
        &mut engine,
        CallRequest::new("add_office").arg("company", text("acme")).arg("id", text("hq")).arg("city", text("Paris")),
    );
    call(
        &mut engine,
        CallRequest::new("add_office").arg("company", text("acme")).arg("id", text("lab")).arg("city", text("Lyon")),
    );
    for (office, desk, label) in [("hq", "d1", "north"), ("hq", "d2", "south"), ("lab", "d1", "bench")] {
        call(
            &mut engine,
            CallRequest::new("add_desk")
                .arg("company", text("acme"))
                .arg("office", text(office))
                .arg("id", text(desk))
                .arg("label", text(label)),
        );
    }
    engine
}

/// Every `(key, value)` a view projects for `field`, paired with the row's own
/// identifying field, so an assertion names rows rather than positions.
fn projected(engine: &Engine<MemoryStore>, view: &str, key: &str, field: &str) -> Vec<(Value, Value)> {
    let rows = engine.view_at_head(view).expect("view").expect("the view is declared");
    rows.rows()
        .iter()
        .map(|row| {
            (
                row.field(key).cloned().expect("the key field is projected"),
                row.field(field).cloned().expect("the field is projected"),
            )
        })
        .collect()
}

/// A compatible update copies every nested row forward verbatim, at depth 2 and
/// depth 3 alike — the §20.1 compatible same-identity copy is over committed
/// rows, not over top-level rows.
#[test]
fn compatible_update_carries_nested_rows_at_every_depth() {
    let mut engine = populated("nestmig", DESKS_V1);
    let mut generator = generator();

    let report = engine.update(DESKS_V1_1, &mut generator).expect("the update commits");
    assert_eq!(report.relation, UpdateRelation::Minor, "1.0.0 -> 1.1.0 is a minor update");

    assert_eq!(
        projected(&engine, "all_offices", "office", "city"),
        vec![(text("hq"), text("Paris")), (text("lab"), text("Lyon"))],
        "every depth-2 office row survives the update with its value"
    );
    assert_eq!(
        projected(&engine, "all_desks", "desk", "label"),
        vec![(text("d1"), text("north")), (text("d2"), text("south")), (text("d1"), text("bench"))],
        "every depth-3 desk row survives the update with its value"
    );
}

/// §5.1/§20.1: a field ADDED to a nested collection takes its insertion default on
/// every carried child row, exactly as it does on a top-level row.
#[test]
fn added_nested_field_takes_its_default_on_carried_rows() {
    let mut engine = populated("nestdefault", DESKS_V1);
    let mut generator = generator();
    engine.update(DESKS_V1_1, &mut generator).expect("the update commits");

    assert!(
        projected(&engine, "all_offices", "office", "floor").iter().all(|(_, floor)| *floor == int(7)),
        "the added depth-2 field takes its default on every carried row"
    );
    assert!(
        projected(&engine, "all_desks", "desk", "seats").iter().all(|(_, seats)| *seats == int(1)),
        "the added depth-3 field takes its default on every carried row"
    );
}

/// §20.1: a `$from`/`$as` mapping declared on a NESTED collection's field
/// transforms every carried child row, the nested analogue of a top-level rename.
#[test]
fn nested_from_as_mapping_transforms_carried_rows() {
    let mut engine = populated("nestrename", DESKS_V1);
    let mut generator = generator();
    engine.update(DESKS_V2, &mut generator).expect("the major update commits");

    assert_eq!(
        projected(&engine, "all_desks", "desk", "tag"),
        vec![(text("d1"), text("NORTH")), (text("d2"), text("SOUTH")), (text("d1"), text("BENCH"))],
        "the nested $from/$as mapping transforms every carried child row"
    );
}

/// §5.8: a top-level collection ADOPTED through a `$types` name is a first-class
/// collection, so its rows — and its own nested children — carry through an update
/// like a directly-declared one. The capture used to select `Node::Collection`
/// members only, silently dropping this whole shape.
const ADOPTED_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.adoptmig@1.0.0"
  "$types": {
    "company": {
      "$key": "id"
      "id": "text"
      "offices": { "$key": "id", "id": "text", "city": "text" }
    }
  }
  "$model": {
    "companies": "company"
    "all_offices": { "$view": ".companies[:c].offices[:o] { office: o.id, city: o.city }" }
    "$mut": {
      "add_company": ".companies + { id: @id }"
      "add_office": ".companies[@company].offices + { id: @id, city: @city }"
    }
  }
}"#;

const ADOPTED_V1_1: &str = r#"{
  "$liasse": 1
  "$app": "t.adoptmig@1.1.0"
  "$types": {
    "company": {
      "$key": "id"
      "id": "text"
      "offices": { "$key": "id", "id": "text", "city": "text", "floor": "int = 2" }
    }
  }
  "$model": {
    "companies": "company"
    "all_offices": { "$view": ".companies[:c].offices[:o] { office: o.id, city: o.city, floor: o.floor }" }
    "$mut": {
      "add_company": ".companies + { id: @id }"
      "add_office": ".companies[@company].offices + { id: @id, city: @city }"
    }
  }
}"#;

#[test]
fn update_carries_rows_of_a_types_adopted_top_level_collection() {
    let mut engine = load("adoptmig", ADOPTED_V1);
    call(&mut engine, CallRequest::new("add_company").arg("id", text("acme")));
    call(
        &mut engine,
        CallRequest::new("add_office").arg("company", text("acme")).arg("id", text("hq")).arg("city", text("Paris")),
    );
    let mut generator = generator();
    engine.update(ADOPTED_V1_1, &mut generator).expect("the update commits");

    assert_eq!(
        projected(&engine, "all_offices", "office", "city"),
        vec![(text("hq"), text("Paris"))],
        "a §5.8-adopted collection's nested rows carry through the update"
    );
    assert_eq!(
        projected(&engine, "all_offices", "office", "floor"),
        vec![(text("hq"), int(2))],
        "and take the added field's default"
    );
}

/// §4.1/§13.13: `$bundle` is package-authoritative and three-way merges on update
/// — including a bundled row nested under a parent. The old bundled value is
/// replaced where the instance still holds it; a locally edited value is retained.
const BUNDLED_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.nestbundle@1.0.0"
  "$model": {
    "manuals": {
      "$key": "id"
      "id": "text"
      "pages": { "$key": "id", "id": "text", "body": "text" }
    }
    "all_pages": { "$view": ".manuals[:m].pages[:p] { page: p.id, body: p.body }" }
    "$mut": { "edit_page": ".manuals[@manual].pages[@page].body = @body" }
  }
  "$bundle": {
    "manuals": { "onboarding": { "pages": { "intro": { "body": "welcome" }, "faq": { "body": "ask us" } } } }
  }
}"#;

const BUNDLED_V1_1: &str = r#"{
  "$liasse": 1
  "$app": "t.nestbundle@1.1.0"
  "$model": {
    "manuals": {
      "$key": "id"
      "id": "text"
      "pages": { "$key": "id", "id": "text", "body": "text" }
    }
    "all_pages": { "$view": ".manuals[:m].pages[:p] { page: p.id, body: p.body }" }
    "$mut": { "edit_page": ".manuals[@manual].pages[@page].body = @body" }
  }
  "$bundle": {
    "manuals": { "onboarding": { "pages": { "intro": { "body": "welcome aboard" }, "faq": { "body": "ask anyone" } } } }
  }
}"#;

#[test]
fn bundle_merge_reaches_a_nested_bundled_row() {
    let mut engine = load("nestbundle", BUNDLED_V1);
    // A local edit to ONE bundled nested row; the other keeps the bundled value.
    call(
        &mut engine,
        CallRequest::new("edit_page")
            .arg("manual", text("onboarding"))
            .arg("page", text("faq"))
            .arg("body", text("mine")),
    );
    let mut generator = generator();
    engine.update(BUNDLED_V1_1, &mut generator).expect("the update commits");

    assert_eq!(
        projected(&engine, "all_pages", "page", "body"),
        vec![(text("faq"), text("mine")), (text("intro"), text("welcome aboard"))],
        "the untouched nested bundled row takes the new release's value; the locally edited one is retained"
    );
}
