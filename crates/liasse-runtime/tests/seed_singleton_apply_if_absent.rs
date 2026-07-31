#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13.13 / §8.2: a `$seed` value at a ROOT-SINGLETON address applies on update
//! WHERE ABSENT, exactly as a `$seed` row at an absent collection address does.
//!
//! §13.13: "**`$seed` applies where absent.** A seed value applies only where its
//! address holds no current value... An existing row, field, or member is never
//! modified by `$seed`". The rule names an ADDRESS and excludes no container, and
//! §8.2 makes a writable non-collection root member durable state at its own
//! address — so the two directions of the rule hold at the root too:
//!
//!   * ABSENT address  -> the newly seeded value is INSERTED;
//!   * PRESENT address -> the current value is RETAINED, never overwritten.
//!
//! This is the `$seed` analogue of the `$bundle` root-singleton defect fixed in
//! `bundle_singleton_merge.rs`: `SeedMode::ApplyIfAbsent` materialized only
//! keyed-collection rows and skipped every root-singleton member, so a `$seed`
//! value at a singleton address that the §20.1 compatible copy left empty was
//! never filled — a package could declare starting root state that no instance
//! updating into that release would ever receive.

mod support;

use liasse_runtime::{CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

/// Two optional root members (§8.2), a root mutation that edits one, and a root
/// view that reads both. Every member is optional so the case isolates the
/// apply-if-absent rule rather than required-member admission (§5.1).
const MODEL: &str = r#"
    "$model": {
      "motto": "text?",
      "theme": "text?",
      "$mut": { "set_theme": [".theme = @v", "return . { theme }"] },
      "settings": { "$view": ". { motto, theme }" }
    }"#;

fn definition(version: &str, seed: &str) -> String {
    format!(r#"{{ "$liasse": 1, "$app": "t.seedsing@{version}",{MODEL}, "$seed": {seed} }}"#)
}

/// The value the `settings` root view reports for member `name`.
fn member(engine: &Engine<MemoryStore>, name: &str) -> Option<serde_json::Value> {
    let view = engine.view_at_head("settings").expect("view evaluates").expect("`settings` is declared");
    view.rows()
        .first()
        .and_then(|row| row.field(name))
        .map(Value::to_wire)
        .filter(|wire| !wire.is_null())
}

fn edit_theme(engine: &mut Engine<MemoryStore>, value: &str) {
    let mut generator = generator();
    let request = CallRequest::new("set_theme").arg("v", Value::Text(Text::new(value)));
    engine.call(&request, &mut generator).expect("the root mutation commits");
}

/// REPRODUCTION (§13.13 ABSENT -> insert): a `$seed` member the release NEWLY
/// declares reaches a singleton address the instance holds nothing at.
///
/// Before the fix `motto` stayed absent after the update: `SeedMode::ApplyIfAbsent`
/// never visited root-singleton members at all.
#[test]
fn seed_fills_an_absent_singleton_member_on_update() {
    let mut engine = load("seed-sing-absent", &definition("1.0.0", r#"{ "theme": "T1" }"#));
    let mut generator = generator();
    assert_eq!(member(&engine, "motto"), None, "the 1.0.0 seed carries no `motto`");

    engine
        .update(&definition("1.1.0", r#"{ "theme": "T1", "motto": "M2" }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        member(&engine, "motto"),
        Some(serde_json::json!("M2")),
        "§13.13: `$seed` applies where absent — the singleton address held no value, so the newly \
         seeded one is inserted",
    );
}

/// §13.13 PRESENT -> retain: a `$seed` member whose address already holds a value
/// is never overwritten, even when the release changed the seeded value.
#[test]
fn seed_never_overwrites_a_present_singleton_member_on_update() {
    let mut engine = load("seed-sing-present", &definition("1.0.0", r#"{ "theme": "T1" }"#));
    let mut generator = generator();

    engine
        .update(&definition("1.1.0", r#"{ "theme": "T2" }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        member(&engine, "theme"),
        Some(serde_json::json!("T1")),
        "§13.13: once a value is present at a seeded address, later seed changes do not touch it",
    );
}

/// The same retain rule against a LOCAL edit: user data at a seeded singleton
/// address survives a release that re-declares that address.
#[test]
fn seed_never_overwrites_a_locally_edited_singleton_member() {
    let mut engine = load("seed-sing-edited", &definition("1.0.0", r#"{ "theme": "T1" }"#));
    edit_theme(&mut engine, "mine");
    let mut generator = generator();

    engine
        .update(&definition("1.1.0", r#"{ "theme": "T2", "motto": "M2" }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        member(&engine, "theme"),
        Some(serde_json::json!("mine")),
        "§13.13: user data at a seeded address is never modified by `$seed`",
    );
    assert_eq!(
        member(&engine, "motto"),
        Some(serde_json::json!("M2")),
        "the sibling absent address still receives its newly seeded value",
    );
}

/// §13.15/§D.3: a singleton member the `$seed` pass filled is a `$seeded` item at
/// its NAME-ONLY application path — the reserved storage row is not part of the
/// address space and must never appear in the report.
#[test]
fn seeded_report_names_the_filled_singleton_member() {
    let mut engine = load("seed-sing-report", &definition("1.0.0", r#"{ "theme": "T1" }"#));
    let mut generator = generator();

    let report = engine
        .update(&definition("1.1.0", r#"{ "theme": "T1", "motto": "M2" }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert!(
        report.seeded.contains(&"/motto".to_owned()),
        "§13.15: the filled singleton member is a `$seeded` item, got {:?}",
        report.seeded,
    );
    assert!(
        !report.seeded.iter().any(|path| path.contains("$root")),
        "§D.3: the reserved singleton storage row never appears in a display path, got {:?}",
        report.seeded,
    );
}

/// GENESIS control: `$seed` still applies as an ordinary insert at first
/// installation (§13.13), root-singleton members included.
#[test]
fn genesis_seed_still_applies_as_an_insert() {
    let engine = load("seed-sing-genesis", &definition("1.0.0", r#"{ "motto": "M1", "theme": "T1" }"#));
    assert_eq!(member(&engine, "motto"), Some(serde_json::json!("M1")));
    assert_eq!(member(&engine, "theme"), Some(serde_json::json!("T1")));
}
