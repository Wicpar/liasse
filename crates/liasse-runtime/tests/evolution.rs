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

/// §20.1 package-level `$migrations` program: a target keyed to the exact active
/// source splits `users` into `people` (every user) and `emails` (only users with
/// an email), reading the read-only `$old` source state.
const SPLIT_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.split@1.0.0"
  "$model": {
    "users": { "$key": "id", "id": "text", "name": "text", "email": "text?" }
    "all_users": { "$view": ".users { id, name }" }
  }
  "$data": {
    "users": {
      "u1": { "name": "  Ann  ", "email": "ann@example.com" }
      "u2": { "name": "Bob" }
    }
  }
}"#;

const SPLIT_V2: &str = r#"{
  "$liasse": 1
  "$app": "t.split@2.0.0"
  "$model": {
    "people": { "$key": "id", "id": "text", "display_name": "text" }
    "emails": { "$key": "user", "user": "text", "email": "text" }
    "$migrations": {
      "1.0.0": [
        ".people = $old.users { id, display_name: string.trim(.name) }",
        ".emails = $old.users[:u | has(u.email)] { user: .id, email: .email }"
      ]
    }
    "people_view": { "$view": ".people { id, display_name, $sort: [id] }" }
    "emails_view": { "$view": ".emails { user, email, $sort: [user] }" }
  }
}"#;

#[test]
fn migrations_program_splits_a_collection_for_the_active_source() {
    let mut engine = load("split", SPLIT_V1);
    let mut generator = generator();

    engine.update(SPLIT_V2, &mut generator).expect("the $migrations program commits");

    // §20.1: the program mapped every user into `people` (trimming the name) and
    // only users with an email into `emails`.
    let people = engine.view_at_head("people_view").expect("view").expect("declared");
    assert_eq!(people.len(), 2, "every user becomes a person");
    assert_eq!(people.rows()[0].field("display_name"), Some(&text("Ann")), "program trims the name");
    assert_eq!(people.rows()[1].field("display_name"), Some(&text("Bob")));
    let emails = engine.view_at_head("emails_view").expect("view").expect("declared");
    assert_eq!(emails.len(), 1, "only u1 has an email");
    assert_eq!(emails.rows()[0].field("user"), Some(&text("u1")));
    assert_eq!(emails.rows()[0].field("email"), Some(&text("ann@example.com")));
}

/// A byte-identical replay of the 2.0.0 load: the "1.0.0"-keyed program cannot
/// re-fire once 2.0.0 is active, so the same-identity `people` are copied, not
/// re-derived from a now-absent `$old.users` (§20.1 exact-source key).
#[test]
fn migrations_program_does_not_refire_on_identical_replay() {
    let mut engine = load("split", SPLIT_V1);
    let mut generator = generator();
    engine.update(SPLIT_V2, &mut generator).expect("first migration commits");

    let report = engine.update(SPLIT_V2, &mut generator).expect("replay is admissible");
    assert_eq!(report.relation, UpdateRelation::SameVersion, "2.0.0 -> 2.0.0 is a same-version republish");
    let people = engine.view_at_head("people_view").expect("view").expect("declared");
    assert_eq!(people.len(), 2, "the same-identity people are copied, not doubled");
    assert_eq!(people.rows()[0].field("display_name"), Some(&text("Ann")), "value unchanged by the replay");
}

/// §20.1: a delta program "MAY read any `$old` view". A declared top-level `$view`
/// (§7) that filters and projects is the canonical view; the delta reads and
/// projects it. `big` keeps only items with `qty > 5`, so the migrated `kept`
/// holds exactly those, proving the delta reads the FILTERED, PROJECTED view rows.
const OLDVIEW_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.oldview@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "qty": "int" }
    "big": { "$view": ".items[:i | i.qty > 5] { id, qty }" }
    "items_view": { "$view": ".items { id, qty }" }
  }
  "$data": { "items": { "i1": { "qty": "5" }, "i2": { "qty": "7" }, "i3": { "qty": "9" } } }
}"#;

const OLDVIEW_V2: &str = r#"{
  "$liasse": 1
  "$app": "t.oldview@2.0.0"
  "$model": {
    "kept": { "$key": "id", "id": "text", "qty": "int" }
    "$migrations": { "1.0.0": [ ".kept = $old.big { id, qty }" ] }
    "kept_view": { "$view": ".kept { id, qty, $sort: [id] }" }
  }
}"#;

#[test]
fn migration_program_reads_and_projects_an_old_top_level_view() {
    let mut engine = load("oldview", OLDVIEW_V1);
    let mut generator = generator();
    engine.update(OLDVIEW_V2, &mut generator).expect("reading a top-level $old view migrates");
    let kept = engine.view_at_head("kept_view").expect("view").expect("declared");
    // i1 (qty 5) is filtered out by the `big` view (`qty > 5`); i2 and i3 survive.
    assert_eq!(kept.len(), 2, "only items with qty > 5 flow through the source view");
    assert_eq!(kept.rows()[0].field("id"), Some(&text("i2")));
    assert_eq!(kept.rows()[0].field("qty"), Some(&int(7)));
    assert_eq!(kept.rows()[1].field("id"), Some(&text("i3")));
    assert_eq!(kept.rows()[1].field("qty"), Some(&int(9)));
}

/// §20.1 final refs check: a program that sets every player's `team` to a literal
/// key present in no `teams` row builds a dangling ref, so the prospective target
/// fails the refs check and the update is rejected (§5.6).
const REF_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.ref@1.0.0"
  "$model": {
    "teams": { "$key": "id", "id": "text", "name": "text" }
    "players": { "$key": "id", "id": "text", "team": { "$ref": "/teams" } }
    "players_view": { "$view": ".players { id, team }" }
  }
  "$data": { "teams": { "t1": { "name": "Red" } }, "players": { "p1": { "team": "t1" } } }
}"#;

const REF_V2: &str = r#"{
  "$liasse": 1
  "$app": "t.ref@2.0.0"
  "$model": {
    "teams": { "$key": "id", "id": "text", "name": "text" }
    "players": { "$key": "id", "id": "text", "team": { "$ref": "/teams" } }
    "$migrations": {
      "1.0.0": [
        ".teams = $old.teams { id, name }",
        ".players = $old.players { id, team: \"ghost\" }"
      ]
    }
    "players_view": { "$view": ".players { id, team }" }
  }
}"#;

#[test]
fn migration_producing_a_dangling_ref_is_rejected() {
    let mut engine = load("ref", REF_V1);
    let mut generator = generator();
    match engine.update(REF_V2, &mut generator) {
        // §20.1/§5.6: `team = "ghost"` resolves to no `/teams` row.
        Err(UpdateError::Rejected(_)) => {}
        other => panic!("a dangling-ref migration must be rejected, got {other:?}"),
    }
    // §E.9: 1.0.0 stays active with its resolving ref intact.
    assert_eq!(engine.model().header().identity.version.major, 1, "1.0.0 stays active");
    let players = engine.view_at_head("players_view").expect("view").expect("declared");
    assert_eq!(players.len(), 1, "the prior players collection is intact");
}

/// §20.1: a field `$from` naming a source field the source collection does not
/// declare resolves to no source and rejects (the confusable / typo case), rather
/// than silently leaving the target field unpopulated.
const FROM_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.from@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "name": "text" }
    "items_view": { "$view": ".items { id, name }" }
  }
  "$data": { "items": { "i1": { "name": "real" } } }
}"#;

const FROM_V2_BAD: &str = r#"{
  "$liasse": 1
  "$app": "t.from@2.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "label": { "$type": "text", "$from": "nonexistent" } }
    "items_view": { "$view": ".items { id, label }" }
  }
}"#;

#[test]
fn migration_from_naming_a_nonexistent_source_field_is_rejected() {
    let mut engine = load("from", FROM_V1);
    let mut generator = generator();
    match engine.update(FROM_V2_BAD, &mut generator) {
        // §20.1: `$from: "nonexistent"` names no field of `items`.
        Err(UpdateError::Rejected(_)) => {}
        other => panic!("a `$from` naming no source field must reject, got {other:?}"),
    }
    let items = engine.view_at_head("items_view").expect("view").expect("declared");
    assert_eq!(items.rows()[0].field("name"), Some(&text("real")), "1.0.0 stays active");
}

/// §20.2 reversible transform via the built-in codec namespaces (§16.1): `$as`
/// base64-encodes the UTF-8 bytes of the old value and `$back` inverts it, so the
/// verified round trip commits and `encoded` holds the canonical base64.
const B64_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.b64@1.0.0"
  "$model": {
    "accounts": { "$key": "id", "id": "text", "name": "text" }
    "all": { "$view": ".accounts { id, name }" }
  }
  "$data": { "accounts": { "a1": { "name": "hi" } } }
}"#;

const B64_V2: &str = r#"{
  "$liasse": 1
  "$app": "t.b64@2.0.0"
  "$model": {
    "accounts": {
      "$key": "id"
      "id": "text"
      "encoded": {
        "$type": "text"
        "$from": "name"
        "$as": "base64.encode(string.bytes(.))"
        "$back": "string.from_bytes(base64.decode(.))"
      }
    }
    "all": { "$view": ".accounts { id, encoded }" }
  }
}"#;

#[test]
fn base64_reversible_transform_commits() {
    let mut engine = load("b64", B64_V1);
    let mut generator = generator();
    engine.update(B64_V2, &mut generator).expect("the reversible base64 migration commits");
    let view = engine.view_at_head("all").expect("view").expect("declared");
    // base64.encode(string.bytes("hi")) == "aGk=" (bytes 0x68 0x69).
    assert_eq!(view.rows()[0].field("encoded"), Some(&text("aGk=")), "hi encodes to aGk=");
}

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
