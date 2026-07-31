#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §22.1/§5.1/§8.2: a required §8.2 root-singleton member must carry a value in
//! EVERY committed state — including the very first one.
//!
//! # The asymmetry this pins shut
//!
//! §22.1 lists "field and shape types" among the state constraints that "hold in
//! every committed state". A non-optional root member declared `"motto": "text"`
//! therefore has no admissible committed state in which it holds nothing: `none`
//! is absence, not a `text` value (§5.5/Annex A.1).
//!
//! The update path already enforced exactly that — `build_migrated` refuses a
//! prospective target whose required field is unpopulated — while GENESIS admitted
//! the same shape happily. The result was an instance that was born inadmissible
//! and could never be updated: creation succeeded, and the FIRST compatible update
//! (even a byte-identical shape at a higher version) was rejected with "migration
//! left required field `motto` unpopulated", blaming a migration that changed
//! nothing. The trap was sprung at update time by a defect at creation time.
//!
//! §9.1 gives the resolution: seed data "passes through the same defaults,
//! normalization, checks, key, ref, uniqueness, bucket, and meter rules as mutation
//! inserts", and §8.2 makes a writable root member durable state at its own
//! address. So genesis is judged by the same population rule the update path
//! applies, and an unpopulated required member is fail-closed AT CREATION.
//!
//! An OPTIONAL member (`"text?"`) stays legal absent, and a required member the
//! package itself populates — a `$bundle`/`$seed` value or a `= default` — is
//! legal, because the state that commits does carry a value.

mod support;

use liasse_runtime::Engine;
use support::{generator, load, store};

/// A model with one REQUIRED root member (`motto`, no default) and one optional
/// bundled member, so the reserved singleton row exists while `motto` holds
/// nothing — the shape whose first update used to reject.
fn required_member(version: &str, bundle: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "t.reqsing@{version}",
          "$model": {{
            "motto": "text",
            "theme": "text?",
            "settings": {{ "$view": ". {{ motto, theme }}" }}
          }},
          "$bundle": {bundle}
        }}"#
    )
}

/// REPRODUCTION: genesis must refuse an unpopulated required singleton member.
///
/// Before the fix this load SUCCEEDED (`Engine::load` returned `Ok`), because the
/// genesis path ran no population check at all over the §8.2 reserved row.
#[test]
fn genesis_rejects_an_unpopulated_required_singleton_member() {
    let mut generator = generator();
    let error = Engine::load(store("req-sing-genesis"), &required_member("1.0.0", r#"{ "theme": "T1" }"#), &mut generator)
        .err()
        .map(|error| error.to_string());

    assert!(
        error.is_some(),
        "§22.1/§5.1: `motto` is a non-optional root member with no default and no `$seed`/`$bundle` \
         value, so no committed state satisfies its declared type — genesis must reject rather than \
         create an instance whose first update is refused for a migration that changed nothing",
    );
    let error = error.unwrap_or_default();
    assert!(error.contains("motto"), "the rejection must name the unpopulated member, got: {error}");
}

/// The same shape with the reserved row entirely EMPTY (nothing bundled at all):
/// the instance holds no root row, and every required member is still unpopulated.
/// §8.2 gives the instance exactly one root whether or not any member is written,
/// so the absence of the storage row is not an escape from §22.1.
#[test]
fn genesis_rejects_a_required_singleton_member_with_no_root_row_at_all() {
    let definition = r#"{
      "$liasse": 1,
      "$app": "t.reqsing.empty@1.0.0",
      "$model": {
        "motto": "text",
        "settings": { "$view": ". { motto }" }
      }
    }"#;
    let mut generator = generator();
    let error = Engine::load(store("req-sing-empty"), definition, &mut generator).err().map(|e| e.to_string());

    assert!(
        error.is_some(),
        "§8.2/§22.1: an instance holds exactly one root; a required member of it is unpopulated \
         whether or not any sibling member caused the reserved row to materialize",
    );
}

/// An OPTIONAL root member stays legal while absent — the fix is fail-closed on
/// the declared shape, not on absence itself (§5.1).
#[test]
fn genesis_admits_an_absent_optional_singleton_member() {
    let definition = r#"{
      "$liasse": 1,
      "$app": "t.optsing@1.0.0",
      "$model": {
        "motto": "text?",
        "settings": { "$view": ". { motto }" }
      }
    }"#;
    let engine = load("opt-sing", definition);
    let view = engine.view_at_head("settings").expect("view evaluates").expect("`settings` is declared");
    assert_eq!(
        view.rows().first().and_then(|row| row.field("motto")).map(liasse_runtime::Value::to_wire),
        None,
        "§5.1: an optional member holding nothing is an admissible committed state, so genesis \
         admits and the member simply reads absent",
    );
}

/// A required member the package POPULATES — here through `$bundle` — is admitted
/// at genesis and stays admissible across the update the old asymmetry blocked.
#[test]
fn a_populated_required_singleton_member_admits_and_updates() {
    let mut engine = load("req-sing-ok", &required_member("1.0.0", r#"{ "motto": "M1", "theme": "T1" }"#));
    let mut generator = generator();

    engine
        .update(&required_member("1.1.0", r#"{ "motto": "M2", "theme": "T1" }"#), &mut generator)
        .expect("a compatible minor update of a fully populated instance commits");
}

/// A required member with a `= default` is populated by §5.1's insertion default at
/// genesis, so it admits — the default is the package's own way of populating it.
#[test]
fn genesis_admits_a_required_singleton_member_with_a_default() {
    let definition = r#"{
      "$liasse": 1,
      "$app": "t.defsing@1.0.0",
      "$model": {
        "motto": "text = 'hello'",
        "settings": { "$view": ". { motto }" }
      }
    }"#;
    let engine = load("def-sing", definition);
    let view = engine.view_at_head("settings").expect("view evaluates").expect("`settings` is declared");
    assert_eq!(
        view.rows().first().and_then(|row| row.field("motto")).map(liasse_runtime::Value::to_wire),
        Some(serde_json::json!("hello")),
        "§5.1: the insertion default populates the required member at genesis",
    );
}
