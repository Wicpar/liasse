#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Annex E / §20.3 boundary-contract narrowing on package update.
//!
//! Within one package major, a minor or patch update MUST preserve or widen every
//! exposed boundary contract (E.1). The engine rejects a narrowing release before
//! activation, leaving the current package active (E.9); a widening or additive
//! release commits. Each case re-derives its outcome from the Annex E text alone.

mod support;

use liasse_runtime::{Engine, RejectionReason, UpdateError};
use liasse_store::MemoryStore;
use support::{generator, load};

/// Assert an update is refused as a boundary-contract narrowing (E.9).
fn assert_narrowing(engine: &mut Engine<MemoryStore>, target: &str) -> String {
    let mut generator = generator();
    match engine.update(target, &mut generator) {
        Err(UpdateError::Rejected(rejection)) => {
            assert_eq!(
                rejection.reason(),
                RejectionReason::Compatibility,
                "a boundary narrowing is a compatibility rejection, got: {}",
                rejection.message()
            );
            rejection.message().to_owned()
        }
        other => panic!("expected a compatibility narrowing rejection, got {other:?}"),
    }
}

/// Assert an update commits (a preserve-or-widen or additive release).
fn assert_commits(engine: &mut Engine<MemoryStore>, target: &str) {
    let mut generator = generator();
    engine.update(target, &mut generator).expect("compatible update commits");
}

// --- E.5 output-shape narrowing (view) --------------------------------------

const RMOUT_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.rmout@1.0.0"
  "$model": {
    "companies": { "$key": "id", "id": "text", "name": "text" }
    "$public": { "companies": { "$view": ".companies { id, name }" } }
  }
  "$data": { "companies": { "acme": { "name": "Acme" } } }
}"#;

/// E.5 breaking: "removing or renaming an output member." The public view drops
/// `name`, narrowing the declared boundary output.
#[test]
fn minor_removes_public_output_member_rejected() {
    let mut engine = load("rmout", RMOUT_V1);
    let target = RMOUT_V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#".companies { id, name }"#, r#".companies { id }"#);
    let message = assert_narrowing(&mut engine, &target);
    assert!(message.contains("name"), "diagnostic names the removed member: {message}");
    // E.9: the prior release remains active and still promises `name`.
    assert_eq!(engine.model().header().identity.version.minor, 0, "1.0.0 stays active");
}

/// E.5 compatible: "adding an optional output field." Widening the projection is
/// substitutable, so the update commits.
#[test]
fn minor_adds_optional_output_field_committed() {
    let mut engine = load("optout", RMOUT_V1);
    let target = RMOUT_V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""name": "text""#, r#""name": "text", "region": "text?""#)
        .replace(r#".companies { id, name }"#, r#".companies { id, name, region }"#);
    assert_commits(&mut engine, &target);
    assert_eq!(engine.model().header().identity.version.minor, 1, "1.1.0 is active");
}

/// E.5 breaking: "making a required output optional." The projected `name` was
/// required (from `name: text`); the candidate declares `name: text?`.
#[test]
fn minor_makes_required_output_optional_rejected() {
    let mut engine = load("reqopt", RMOUT_V1);
    let target = RMOUT_V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""name": "text""#, r#""name": "text?""#);
    assert_narrowing(&mut engine, &target);
}

// --- E.5 exhaustive enum result ---------------------------------------------

const ENUM_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.enum@1.0.0"
  "$model": {
    "tickets": { "$key": "id", "id": "text", "status": { "$enum": ["draft", "active", "closed"] } }
    "$public": { "tickets": { "$view": ".tickets { id, status }" } }
  }
}"#;

/// E.5: an exhaustive output enum that loses the promised label `closed` narrows
/// the result domain and is rejected.
#[test]
fn minor_removes_enum_result_label_rejected() {
    let mut engine = load("enumout", ENUM_V1);
    let target = ENUM_V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#"["draft", "active", "closed"]"#, r#"["draft", "active"]"#);
    assert_narrowing(&mut engine, &target);
}

/// E.5: widening an exhaustively declared enum *result* is also breaking, so an
/// added output label is rejected (the output enum domain must stay identical).
#[test]
fn minor_widens_enum_result_label_rejected() {
    let mut engine = load("enumwiden", ENUM_V1);
    let target = ENUM_V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#"["draft", "active", "closed"]"#, r#"["draft", "active", "closed", "archived"]"#);
    assert_narrowing(&mut engine, &target);
}

// --- E.5 exposed row identity -----------------------------------------------

const IDENT_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.ident@1.0.0"
  "$model": {
    "companies": {
      "$key": "id", "id": "text", "code": "text", "name": "text", "$unique": ["code"]
    }
    "$public": { "companies": { "$view": ".companies { id, code, name }" } }
  }
  "$data": { "companies": { "acme": { "code": "AC", "name": "Acme" } } }
}"#;

/// E.5: "changing exposed row identity." The projection keeps the same field set
/// but re-keys `companies` from `id` to `code`, so the exposed identity changes.
#[test]
fn minor_changes_exposed_identity_rejected() {
    let mut engine = load("ident", IDENT_V1);
    let target = r#"{
  "$liasse": 1
  "$app": "t.compat.ident@1.1.0"
  "$model": {
    "companies": {
      "$key": "code", "id": "text", "code": "text", "name": "text", "$unique": ["id"]
    }
    "$public": { "companies": { "$view": ".companies { id, code, name }" } }
  }
  "$data": { "companies": { "AC": { "id": "acme", "name": "Acme" } } }
}"#;
    let message = assert_narrowing(&mut engine, target);
    assert!(message.contains("identity"), "diagnostic reports the identity change: {message}");
}

// --- E.4 input (parameter) contracts ----------------------------------------

const PARAM_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.param@1.0.0"
  "$model": {
    "tasks": {
      "$key": "id", "id": "uuid = uuid()", "title": "text",
      "priority": { "$enum": ["low", "high"] }
    }
    "$mut": {
      "add_task": ["t = .tasks + { title: @title }", "return t { id, title }"]
    }
    "$public": {
      "tasks": { "$view": ".tasks { id, title }", "$mut": { "add": ".add_task" } }
    }
  }
}"#;

/// E.4 breaking: "adding a required parameter." The candidate's `add_task` reads
/// `@priority`, inferred required from the non-optional field.
#[test]
fn minor_adds_required_parameter_rejected() {
    let mut engine = load("reqparam", PARAM_V1);
    let target = PARAM_V1.replace("@1.0.0", "@1.1.0").replace(
        r#"["t = .tasks + { title: @title }", "return t { id, title }"]"#,
        r#"["t = .tasks + { title: @title, priority: @priority }", "return t { id, title }"]"#,
    );
    let message = assert_narrowing(&mut engine, &target);
    assert!(message.contains("priority"), "diagnostic names the added parameter: {message}");
}

/// E.4 compatible: "adding an optional parameter with a default." The candidate's
/// `add_task` reads `@note`, inferred optional from `note: text?`.
#[test]
fn minor_adds_optional_parameter_committed() {
    let mut engine = load("optparam", PARAM_V1);
    let target = PARAM_V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""priority": { "$enum": ["low", "high"] }"#, r#""note": "text?""#)
        .replace(
            r#"["t = .tasks + { title: @title }", "return t { id, title }"]"#,
            r#"["t = .tasks + { title: @title, note: @note }", "return t { id, title }"]"#,
        );
    assert_commits(&mut engine, &target);
}

/// A base whose exposed `add` already reads `@priority`, so an enum change to the
/// `priority` field touches only the accepted *input* domain of an existing
/// parameter (the field is not projected by the view, isolating the input side).
const PARAM_ENUM_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.inenum@1.0.0"
  "$model": {
    "tasks": {
      "$key": "id", "id": "uuid = uuid()", "title": "text",
      "priority": { "$enum": ["low", "high"] }
    }
    "$mut": {
      "add_task": ["t = .tasks + { title: @title, priority: @priority }", "return t { id, title }"]
    }
    "$public": {
      "tasks": { "$view": ".tasks { id, title }", "$mut": { "add": ".add_task" } }
    }
  }
}"#;

/// E.4 breaking: "narrowing ... an accepted enum domain." The exposed `add`
/// parameter `@priority` loses the accepted label `high`.
#[test]
fn minor_narrows_input_enum_domain_rejected() {
    let mut engine = load("inenumnarrow", PARAM_ENUM_V1);
    let target = PARAM_ENUM_V1.replace("@1.0.0", "@1.1.0").replace(r#"["low", "high"]"#, r#"["low"]"#);
    let message = assert_narrowing(&mut engine, &target);
    assert!(message.contains("priority"), "diagnostic names the narrowed parameter: {message}");
}

/// E.4 compatible: "widening an ... accepted enum domain." The `@priority` input
/// gains the label `med`; every earlier input stays accepted, so it commits.
#[test]
fn minor_widens_input_enum_domain_committed() {
    let mut engine = load("inenumwiden", PARAM_ENUM_V1);
    let target =
        PARAM_ENUM_V1.replace("@1.0.0", "@1.1.0").replace(r#"["low", "high"]"#, r#"["low", "med", "high"]"#);
    assert_commits(&mut engine, &target);
}

// --- E.7 mutation response --------------------------------------------------

const RESP_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.resp@1.0.0"
  "$model": {
    "companies": {
      "$key": "id", "id": "text", "name": "text",
      "$mut": { "rename": [".name = @name", "return . { id, name }"] }
    }
    "$public": {
      "companies": {
        "$view": ".companies { id, name }"
        "$mut": { "rename": ".companies[@id].rename()" }
      }
    }
  }
  "$data": { "companies": { "acme": { "name": "Acme" } } }
}"#;

/// E.5/E.7 breaking, held on a patch: "removing ... an output member." The
/// exposed mutation response drops `name`. A patch narrows exactly like a minor.
#[test]
fn patch_narrows_mutation_response_rejected() {
    let mut engine = load("resp", RESP_V1);
    let target = RESP_V1
        .replace("@1.0.0", "@1.0.1")
        .replace(r#""return . { id, name }""#, r#""return . { id }""#);
    let message = assert_narrowing(&mut engine, &target);
    assert!(message.contains("response"), "diagnostic reports the response narrowing: {message}");
}

/// E.5: "adding an optional output field" applied to a mutation response commits;
/// every promised response member is preserved.
#[test]
fn patch_widens_mutation_response_committed() {
    let mut engine = load("respwiden", RESP_V1);
    let target = RESP_V1
        .replace("@1.0.0", "@1.0.1")
        .replace(r#""return . { id, name }""#, r#""return . { id, name, upper }""#)
        .replace(r#""name": "text","#, r#""name": "text", "upper": "= string.upper(.name)","#);
    assert_commits(&mut engine, &target);
}

// --- E.4/E.6 additive and private evolution ---------------------------------

/// E.4: "adding a new surface, interface, or mutation name" and E.6 adding a
/// private collection are compatible; the additive minor commits.
#[test]
fn minor_additive_surface_and_private_collection_committed() {
    let mut engine = load("additive", RMOUT_V1);
    let target = r#"{
  "$liasse": 1
  "$app": "t.compat.rmout@1.1.0"
  "$model": {
    "companies": { "$key": "id", "id": "text", "name": "text" }
    "audit": { "$key": "id", "id": "text", "note": "text" }
    "$public": {
      "companies": { "$view": ".companies { id, name }" }
      "audit": { "$view": ".audit { id, note }" }
    }
  }
  "$data": { "companies": { "acme": { "name": "Acme" } } }
}"#;
    assert_commits(&mut engine, target);
}

/// A major release MAY remove an output member (E.1); the narrowing check does not
/// apply across a major, so the update proceeds through the ordinary pipeline.
#[test]
fn major_removes_output_member_committed() {
    let mut engine = load("majorrm", RMOUT_V1);
    let target = RMOUT_V1
        .replace("@1.0.0", "@2.0.0")
        .replace(r#".companies { id, name }"#, r#".companies { id }"#);
    assert_commits(&mut engine, &target);
}
