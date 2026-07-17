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

/// A minor release that adds a `$requires` entry naming a namespace the running
/// host context never registered (§16.2). The requirement is not even called by
/// any view — it must still reject the update.
const PEOPLE_V1_1_NEEDS_NS: &str = r#"{
  "$liasse": 1
  "$app": "example.people@1.1.0"
  "$requires": { "pw": "liasse.password@1" }
  "$model": {
    "people": { "$key": "id", "id": "text", "name": "text" }
    "all_people": { "$view": ".people { id, name }" }
  }
}"#;

#[test]
fn update_requiring_an_unregistered_namespace_rejects_and_keeps_prior_active() {
    // The engine loads with no managed host components (empty registry), so its
    // context cannot supply `liasse.password@1`.
    let mut engine = load("people", PEOPLE_V1);
    let mut generator = generator();
    match engine.update(PEOPLE_V1_1_NEEDS_NS, &mut generator) {
        // §2.1/§16.2: a missing requirement fails the update before activation;
        // the host layer surfaces it as an engine-level requirement error.
        Err(UpdateError::Engine(_)) => {}
        other => panic!("expected a requirement rejection, got {other:?}"),
    }
    // §9.4/§E.9: the prior application remains active and intact.
    assert_eq!(engine.model().header().identity.version.minor, 0, "the active version is still 1.0.0");
    let view = engine.view_at_head("all_people").expect("view").expect("declared");
    assert_eq!(view.len(), 1, "prior state still served");
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

/// §20.2 downgrade representability: a downgrade that would drop a populated field
/// the older shape cannot represent, with no declared transform, is rejected.
const REGION_V1_1: &str = r#"{
  "$liasse": 1
  "$app": "t.region@1.1.0"
  "$model": {
    "companies": { "$key": "id", "id": "text", "name": "text", "region": "text" }
    "all": { "$view": ".companies { id, name, region }" }
  }
  "$data": { "companies": { "acme": { "name": "Acme", "region": "EU" } } }
}"#;

/// The older 1.0.0 release: `region` removed, no downgrade transform.
const REGION_V1_0: &str = r#"{
  "$liasse": 1
  "$app": "t.region@1.0.0"
  "$model": {
    "companies": { "$key": "id", "id": "text", "name": "text" }
    "all": { "$view": ".companies { id, name }" }
  }
}"#;

#[test]
fn downgrade_dropping_a_populated_field_is_rejected() {
    let mut engine = load("region", REGION_V1_1);
    let mut generator = generator();
    match engine.update(REGION_V1_0, &mut generator) {
        // §20.2: the older shape cannot represent live `region` and declares no
        // downgrade transform, so the downgrade is rejected.
        Err(UpdateError::Rejected(_)) => {
            // E.9: the active 1.1.0 stays active with its populated state intact.
            let view = engine.view_at_head("all").expect("view").expect("declared");
            assert_eq!(
                view.rows()[0].field("region"),
                Some(&text("EU")),
                "1.1.0 remains active with the populated region preserved",
            );
        }
        other => panic!("a downgrade dropping populated `region` must be rejected, got {other:?}"),
    }
}

/// §20.2 downgrade via exact inverse: the active package declares `$from`/`$back`
/// on the field it added, so downgrading reconstructs the older field. (The corpus
/// case uses base64; here `string.upper`/`string.lower` is an exact inverse for the
/// stored value.)
const ENCODED_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.enc@1.0.0"
  "$model": {
    "accounts": { "$key": "id", "id": "text", "name": "text" }
    "all": { "$view": ".accounts { id, name }" }
  }
  "$data": { "accounts": { "a1": { "name": "hi" } } }
}"#;

const ENCODED_V2: &str = r#"{
  "$liasse": 1
  "$app": "t.enc@2.0.0"
  "$model": {
    "accounts": {
      "$key": "id"
      "id": "text"
      "encoded": { "$type": "text", "$from": "name", "$as": "string.upper(.)", "$back": "string.lower(.)" }
    }
    "all": { "$view": ".accounts { id, encoded }" }
  }
}"#;

#[test]
fn downgrade_via_exact_inverse_reconstructs_the_older_field() {
    let mut engine = load("enc", ENCODED_V1);
    let mut generator = generator();

    // Upgrade 1.0.0 -> 2.0.0: name -> encoded, with a declared exact inverse.
    let up = engine.update(ENCODED_V2, &mut generator).expect("upgrade commits");
    assert_eq!(up.relation, UpdateRelation::Major, "1.0.0 -> 2.0.0 is a major release");
    let encoded = engine.view_at_head("all").expect("view").expect("declared");
    assert_eq!(encoded.rows()[0].field("encoded"), Some(&text("HI")), "hi encodes to HI");

    // Downgrade 2.0.0 -> 1.0.0: the available exact inverse reconstructs `name`.
    let down = engine.update(ENCODED_V1, &mut generator).expect("downgrade commits via inverse");
    assert_eq!(down.relation, UpdateRelation::Downgrade, "2.0.0 -> 1.0.0 is a downgrade");
    let restored = engine.view_at_head("all").expect("view").expect("declared");
    assert_eq!(
        restored.rows()[0].field("name"),
        Some(&text("hi")),
        "name is reconstructed from encoded via the exact inverse",
    );
}
