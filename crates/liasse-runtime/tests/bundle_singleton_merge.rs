#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13.13 / §8.2: a `$bundle` value at a ROOT-SINGLETON address participates in the
//! update three-way merge exactly as a bundled collection field does.
//!
//! §13.13 scopes the bundle merge by ADDRESS, not by container: "For each bundled
//! scalar or struct field, the new bundle replaces the value only when the current
//! value still equals the old bundle value; otherwise the current value is
//! retained." §8.2 makes a writable non-collection root member durable state at its
//! own address, and §4.1 lets `$bundle` supply it. So the matrix below is the
//! collection-field matrix, one address up.

mod support;

use liasse_runtime::{CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

/// The shared model of every version below: two singleton root members (§8.2), one
/// root mutation that edits one of them, and a root view that reads both.
const MODEL: &str = r#"
    "$model": {
      "motto": "text?",
      "theme": "text?",
      "later": "text?",
      "$mut": { "set_theme": [".theme = @v", "return . { theme }"] },
      "settings": { "$view": ". { motto, theme, later }" }
    }"#;

fn definition(version: &str, bundle: &str) -> String {
    format!(r#"{{ "$liasse": 1, "$app": "t.bundlesing@{version}",{MODEL}, "$bundle": {bundle} }}"#)
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

/// Set `theme` through the root mutation, so the instance holds a LOCAL edit at
/// that singleton address.
fn edit_theme(engine: &mut Engine<MemoryStore>, value: &str) {
    let mut generator = generator();
    let request = CallRequest::new("set_theme").arg("v", Value::Text(Text::new(value)));
    engine.call(&request, &mut generator).expect("the root mutation commits");
}

/// §13.13 case 1: an UNTOUCHED bundled singleton value takes the new bundle value.
///
/// This is the defect the fix closes — before it, `merge_bundle` three-way-walked
/// only `compiled.collection(name)`, so a root-singleton bundle member applied at
/// genesis and was silently skipped on every later update (the §20.1 compatible
/// copy carried the genesis value forward untouched).
#[test]
fn untouched_singleton_member_takes_the_new_bundle_value() {
    let mut engine = load("bundle-sing-untouched", &definition("1.0.0", r#"{ "motto": "M1" }"#));
    let mut generator = generator();
    assert_eq!(member(&engine, "motto"), Some(serde_json::json!("M1")), "genesis applies the bundle");

    engine
        .update(&definition("1.1.0", r#"{ "motto": "M2" }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        member(&engine, "motto"),
        Some(serde_json::json!("M2")),
        "§13.13: the current value still equals the old bundle value, so the new bundle replaces it",
    );
}

/// §13.13 case 2: a LOCALLY EDITED bundled singleton value is retained.
#[test]
fn locally_edited_singleton_member_is_retained() {
    let mut engine = load("bundle-sing-edited", &definition("1.0.0", r#"{ "theme": "T1" }"#));
    let mut generator = generator();
    edit_theme(&mut engine, "mine");

    engine
        .update(&definition("1.1.0", r#"{ "theme": "T2" }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        member(&engine, "theme"),
        Some(serde_json::json!("mine")),
        "§13.13: the current value no longer equals the old bundle value, so the local edit is retained",
    );
}

/// §13.13 case 3: an address the OLD bundle held NOTHING at is filled by the new
/// bundle. Absent-in-the-old-bundle and absent-in-the-instance compare equal
/// (`held`), so a newly bundled member is not mistaken for a local edit.
#[test]
fn newly_bundled_singleton_member_fills() {
    let mut engine = load("bundle-sing-new", &definition("1.0.0", r#"{ "motto": "M1" }"#));
    let mut generator = generator();
    assert_eq!(member(&engine, "later"), None, "the 1.0.0 bundle carries no `later`");

    engine
        .update(&definition("1.1.0", r#"{ "motto": "M1", "later": "L2" }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        member(&engine, "later"),
        Some(serde_json::json!("L2")),
        "§13.13: a newly bundled address whose current value still matches the old bundle (both \
         hold nothing) takes the new bundle value",
    );
}

/// §13.13 case 4: a member the new bundle DROPPED is removed when its current value
/// still equals the old bundled value — the singleton analogue of "a row removed
/// from the new bundle is deleted only when its current subtree still equals the old
/// bundled subtree".
#[test]
fn dropped_singleton_member_is_removed_when_unedited() {
    let mut engine = load("bundle-sing-dropped", &definition("1.0.0", r#"{ "motto": "M1", "later": "L1" }"#));
    let mut generator = generator();
    assert_eq!(member(&engine, "later"), Some(serde_json::json!("L1")));

    engine
        .update(&definition("1.1.0", r#"{ "motto": "M1" }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        member(&engine, "later"),
        None,
        "§13.13: the new bundle dropped `later` and the instance never edited it, so the \
         package-authoritative value is withdrawn",
    );
}

/// §13.13 case 4b: the same drop, but the instance EDITED the value — it is retained
/// as local data, exactly as a locally modified bundled row is.
#[test]
fn dropped_singleton_member_is_retained_when_edited() {
    let mut engine = load("bundle-sing-dropped-edited", &definition("1.0.0", r#"{ "theme": "T1" }"#));
    let mut generator = generator();
    edit_theme(&mut engine, "mine");

    engine
        .update(&definition("1.1.0", r#"{ "motto": "M2" }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        member(&engine, "theme"),
        Some(serde_json::json!("mine")),
        "§13.13: a removed bundled value whose current value diverged is retained as local data",
    );
}

/// §13.13 "for each bundled scalar or **struct** field": a §5.3 static struct at a
/// root-singleton address merges member-by-member under the same rule as a scalar.
#[test]
fn bundled_singleton_struct_member_takes_the_new_bundle_value() {
    const STRUCT_MODEL: &str = r#"
    "$model": {
      "brand": { "name": "text?", "color": "text?" },
      "settings": { "$view": ". { brand }" }
    }"#;
    let definition = |version: &str, bundle: &str| {
        format!(r#"{{ "$liasse": 1, "$app": "t.bundlestruct@{version}",{STRUCT_MODEL}, "$bundle": {bundle} }}"#)
    };
    let brand = |engine: &Engine<MemoryStore>| {
        let view = engine.view_at_head("settings").expect("view evaluates").expect("declared");
        view.rows().first().and_then(|row| row.field("brand")).map(Value::to_wire)
    };

    let mut engine = load(
        "bundle-sing-struct",
        &definition("1.0.0", r#"{ "brand": { "name": "Liasse", "color": "blue" } }"#),
    );
    let mut generator = generator();

    engine
        .update(&definition("1.1.0", r#"{ "brand": { "name": "Liasse", "color": "green" } }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        brand(&engine),
        Some(serde_json::json!({ "color": "green", "name": "Liasse" })),
        "§13.13: an untouched bundled struct member takes the new bundle value",
    );
}

/// §13.15/§D.3: a bundled singleton member is a `$seeded` item, reported at its
/// NAME-ONLY application path (`/motto`). The reserved storage row that holds
/// singleton state is not part of the address space, so neither `$root` nor its
/// placeholder key may appear in the report.
#[test]
fn seeded_report_names_the_singleton_member_not_the_reserved_row() {
    let mut engine = load("bundle-sing-report", &definition("1.0.0", r#"{ "motto": "M1" }"#));
    let mut generator = generator();

    let report = engine
        .update(&definition("1.1.0", r#"{ "motto": "M2" }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(report.seeded, vec!["/motto".to_owned()], "§13.15: the bundled member is a `$seeded` item");
    assert!(
        !report.seeded.iter().any(|path| path.contains("$root")),
        "§D.3: the reserved singleton storage row never appears in a display path, got {:?}",
        report.seeded,
    );
}

/// §13.13/§19.9: when the release and the instance BOTH moved one bundled singleton
/// member, the §20.4 prepared update reports the divergence at that member's
/// name-only §D.3 coordinate — and still resolves it in the instance's favour.
#[test]
fn divergent_singleton_member_is_reported_at_its_member_coordinate() {
    let mut engine = load("bundle-sing-divergent", &definition("1.0.0", r#"{ "theme": "T1" }"#));
    edit_theme(&mut engine, "mine");
    let mut generator = generator();

    let prepared = engine
        .prepare_update(&definition("1.1.0", r#"{ "theme": "T2" }"#), &mut generator)
        .expect("the update prepares");
    let conflicts = &prepared.reconciliation().conflicts;

    assert_eq!(conflicts.len(), 1, "one bundled member moved on both sides, got {conflicts:?}");
    assert_eq!(
        conflicts[0].coordinate,
        liasse_runtime::ConflictCoordinate::RootSingleton { member: Some("theme".to_owned()) },
        "§19.9/§D.3: the coordinate is the member's name-only address, never the reserved row",
    );
    assert_eq!(conflicts[0].kind, liasse_runtime::ConflictKind::IncompatibleValue);

    engine.apply_update(prepared).expect("a reported conflict never blocks the update (§13.13)");
    assert_eq!(
        member(&engine, "theme"),
        Some(serde_json::json!("mine")),
        "§13.13 resolves the divergence in the instance's favour",
    );
}

/// GENESIS control: the bundle still applies as an ordinary insert at first
/// installation (§13.13), and a `$bundle`-only package still resolves the §8.2
/// singleton defaults around it. The merge pass must not disturb either.
#[test]
fn genesis_bundle_still_applies_as_an_insert() {
    let engine = load("bundle-sing-genesis", &definition("1.0.0", r#"{ "motto": "M1", "later": "L1" }"#));
    assert_eq!(member(&engine, "motto"), Some(serde_json::json!("M1")));
    assert_eq!(member(&engine, "later"), Some(serde_json::json!("L1")));
    assert_eq!(member(&engine, "theme"), None, "an unbundled optional member stays absent");
}
