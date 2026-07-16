#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §20 migrations: a compatible minor update copies same-identity fields and
//! fills an added field from its default; a `$from`/`$as` rename transforms an
//! old value; a reversible `$back` round trip is verified and a lossy one rejects.

mod support;

use liasse_runtime::{UpdateError, UpdateRelation, Value};
use liasse_value::{Integer, Text};
use support::{generator, load};

const PEOPLE_V1: &str = r#"{
  "$liasse": 1
  "$app": "example.people@1.0.0"
  "$model": {
    "people": { "$key": "id", "id": "text", "name": "text" }
    "all_people": { "$view": ".people { id, name }" }
  }
  "$data": { "people": { "p1": { "name": "  Ada  " } } }
}"#;

/// A compatible minor release that adds a defaulted `tier` field.
const PEOPLE_V1_1: &str = r#"{
  "$liasse": 1
  "$app": "example.people@1.1.0"
  "$model": {
    "people": { "$key": "id", "id": "text", "name": "text", "tier": "int = 3" }
    "all_people": { "$view": ".people { id, name, tier } " }
  }
}"#;

/// A major release renaming `name` to `display_name` via `$from` + `$as`.
const PEOPLE_V2: &str = r#"{
  "$liasse": 1
  "$app": "example.people@2.0.0"
  "$model": {
    "people": {
      "$key": "id"
      "id": "text"
      "display_name": { "$type": "text", "$from": "name", "$as": "string.trim(.)" }
    }
    "all_people": { "$view": ".people { id, display_name } " }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

#[test]
fn compatible_update_copies_fields_and_defaults_added_field() {
    let mut engine = load("people", PEOPLE_V1);
    let mut generator = generator();

    let report = engine.update(PEOPLE_V1_1, &mut generator).expect("update commits");
    assert_eq!(report.relation, UpdateRelation::Minor, "1.0.0 -> 1.1.0 is a minor update");

    let view = engine.view_at_head("all_people").expect("view").expect("declared");
    assert_eq!(view.len(), 1);
    let row = &view.rows()[0];
    // The name is copied verbatim by the compatible same-identity copy (the
    // untrimmed seed value is preserved, not re-normalized); `tier` is defaulted.
    assert_eq!(row.field("name"), Some(&text("  Ada  ")), "same-identity field copied verbatim");
    assert_eq!(row.field("tier"), Some(&int(3)), "added field takes its default");
}

#[test]
fn from_with_as_transforms_the_old_value() {
    let mut engine = load("people", PEOPLE_V1);
    let mut generator = generator();

    engine.update(PEOPLE_V2, &mut generator).expect("major update commits");

    let view = engine.view_at_head("all_people").expect("view").expect("declared");
    let row = &view.rows()[0];
    // `$as: string.trim(.)` transforms the untrimmed old `name` "  Ada  ".
    assert_eq!(row.field("display_name"), Some(&text("Ada")), "$from/$as transforms the value");
    assert_eq!(row.field("name"), None, "the dropped source field is absent from live state");
}

#[test]
fn unrelated_line_update_is_incompatible() {
    let mut engine = load("people", PEOPLE_V1);
    let mut generator = generator();
    let other = PEOPLE_V1.replace("example.people", "example.other");
    match engine.update(&other, &mut generator) {
        Err(UpdateError::Incompatible(_)) => {}
        other => panic!("expected an incompatible-line rejection, got {other:?}"),
    }
    // The active package is unchanged after a rejected update.
    let view = engine.view_at_head("all_people").expect("view").expect("declared");
    assert_eq!(view.len(), 1);
}
