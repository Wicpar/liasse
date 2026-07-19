#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §5.4 atomic rekey: changing a row's key rewrites every inbound reference to
//! the new key in the same transition and re-validates each referencing row. A
//! rewritten ref that drives a referencing row into a constraint failure rejects
//! the complete transition (§5.4/§5.10); a harmless rekey rewrites the refs and
//! commits. Every expectation is re-derived from §5.4 text.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, RejectionReason, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

/// Teams referenced by members; a member's row check reserves the `banned` key,
/// so rekeying a referenced team onto `banned` must reject through the rewritten
/// inbound ref (§5.4 "resulting constraint failure rejects the complete
/// transition").
const REKEY: &str = r#"{
  "$liasse": 1
  "$app": "t.rekey@1.0.0"
  "$model": {
    "teams": { "$key": "id", "id": "text", "name": "text" }
    "members": {
      "$key": "id",
      "id": "text",
      "team": { "$ref": "/teams" },
      "$check": [".team != 'banned'", "The banned team key is reserved"]
    }
    "members_view": { "$view": ".members { id, team }" }
    "$mut": {
      "add_team": ".teams + { id: @id, name: @name }",
      "add_member": ".members + { id: @id, team: @team }",
      "rekey_team": ".teams[@old].id = @new"
    }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn call(engine: &mut liasse_runtime::Engine<MemoryStore>, name: &str, args: &[(&str, &str)]) -> CallOutcome {
    let mut generator = generator();
    let mut request = CallRequest::new(name);
    for (key, value) in args {
        request = request.arg(*key, text(value));
    }
    engine.call(&request, &mut generator).expect("call")
}

fn member_team(engine: &liasse_runtime::Engine<MemoryStore>, id: &str) -> Option<Value> {
    let view = engine.view_at_head("members_view").expect("view").expect("declared");
    view.rows()
        .iter()
        .find(|row| row.field("id") == Some(&text(id)))
        .and_then(|row| row.field("team").cloned())
}

fn seed(engine: &mut liasse_runtime::Engine<MemoryStore>) {
    assert!(matches!(call(engine, "add_team", &[("id", "good"), ("name", "Good")]), CallOutcome::Committed { .. }));
    assert!(matches!(call(engine, "add_member", &[("id", "m1"), ("team", "good")]), CallOutcome::Committed { .. }));
}

/// §5.4/§5.10: rekeying the referenced team onto the reserved `banned` key
/// rewrites the member's inbound ref, whose row check then fails — so the whole
/// transition is rejected and nothing changes (the team keeps its key; the
/// member keeps its ref).
#[test]
fn rekey_that_violates_a_referencing_row_check_is_rejected() {
    let mut engine = load("rekey-reject", REKEY);
    seed(&mut engine);
    let head = engine.head().unwrap();

    let outcome = call(&mut engine, "rekey_team", &[("old", "good"), ("new", "banned")]);
    let rejection = outcome.rejection().expect("a constraint failure rejects the rekey");
    assert_eq!(rejection.reason(), RejectionReason::Check, "the referencing row's check is what fails");

    assert_eq!(engine.head().unwrap(), head, "the rejected rekey left the frontier intact");
    assert_eq!(member_team(&engine, "m1"), Some(text("good")), "the inbound ref was not rewritten");
}

/// §5.4: a harmless rekey rewrites every inbound reference to the new key and
/// commits — the member now references the renamed team.
#[test]
fn harmless_rekey_rewrites_inbound_refs() {
    let mut engine = load("rekey-ok", REKEY);
    seed(&mut engine);

    let outcome = call(&mut engine, "rekey_team", &[("old", "good"), ("new", "fine")]);
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "a harmless rekey commits");
    assert_eq!(member_team(&engine, "m1"), Some(text("fine")), "the inbound ref was rewritten to the new key");
}
