#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM probe of the Wave-15 set-mutation element decode (commit 25eec41),
//! attacking the seam the landed guards do NOT cover.
//!
//! `redteam_set_ref_key_family_guards` exercises composite/struct-keyed set
//! members but always supplies the operand as a PRE-BUILT `Value::Ref`
//! (`composite_ref(..)` / `struct_ref(..)`), which `Interp::ref_member`
//! (interp.rs:1166) short-circuits unchanged. The DECODE path — a bare
//! application-visible key operand (`Value::Struct` object for a composite key,
//! the struct key value for a struct key) normalized through
//! `normalize_key_operand`/`key_value_of`/`ref_of` — is only proven for a SCALAR
//! key (`redteam_setref_deletion_probe`, via a text literal). These probes drive
//! that decode for the composite and struct key families.
//!
//! §5.5/§5.6/§A.9: a set-of-`$ref` member's authoring value IS the target's key,
//! so an add/remove by that key must store/compare/plan by `Value::Ref` identity.
//! Every expectation is re-derived from spec text.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, RejectionReason, Value};
use liasse_store::MemoryStore;
use liasse_value::{Ref, Struct, Text};
use support::{generator, store};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut g = generator();
    engine.call(request, &mut g).expect("call")
}

fn committed(outcome: CallOutcome) {
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "expected a commit, got {outcome:?}");
}

fn load(instance: &str, pkg: &str) -> Engine<MemoryStore> {
    let mut g = generator();
    Engine::load(store(instance), pkg, &mut g).expect("package loads")
}

fn reviewers(engine: &Engine<MemoryStore>) -> Vec<Value> {
    let view = engine.view_at_head("docs_view").expect("view").expect("declared");
    match view.rows()[0].field("reviewers") {
        Some(Value::Set(members)) => members.iter().cloned().collect(),
        None => Vec::new(),
        other => panic!("reviewers is a set, got {other:?}"),
    }
}

fn count(engine: &Engine<MemoryStore>, view: &str) -> usize {
    engine.view_at_head(view).expect("view").expect("declared").rows().len()
}

fn composite_ref(org: &str, user: &str) -> Value {
    Value::Ref(Ref::composite(vec![text(org), text(user)]))
}

fn struct_key(org: &str, user: &str) -> Value {
    Value::Struct(Struct::new([(Text::new("org"), text(org)), (Text::new("user"), text(user))]))
}

fn struct_ref(org: &str, user: &str) -> Value {
    Value::Ref(Ref::scalar(struct_key(org, user)))
}

// -- Composite key: operand is a bare object, decoded by ref_member ---------

fn composite_pkg(on_delete: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1,
  "$app": "t.setcompdecode@1.0.0",
  "$model": {{
    "accounts": {{ "$key": ["org", "user"], "org": "text", "user": "text" }},
    "docs": {{ "$key": "id", "id": "text",
      "reviewers": {{ "$set": {{ "$ref": "/accounts", "$on_delete": "{on_delete}" }} }} }},
    "docs_view": {{ "$view": ".docs {{ id, reviewers, $sort: [id] }}" }},
    "accounts_view": {{ "$view": ".accounts {{ org, user, $sort: [org, user] }}" }},
    "$mut": {{
      "add_account": ".accounts + {{ org: @org, user: @user }}",
      "add_doc": ".docs + {{ id: @id }}",
      "add_reviewer_obj({{ id: text, org: text, user: text }})": ".docs[@id].reviewers + {{ org: @org, user: @user }}",
      "drop_reviewer_obj({{ id: text, org: text, user: text }})": ".docs[@id].reviewers - {{ org: @org, user: @user }}",
      "delete_account": ".accounts - {{ org: @org, user: @user }}"
    }}
  }}
}}"#
    )
}

/// §5.6/§21.1/§22.1: adding a composite-keyed reviewer through a bare OBJECT
/// operand must store a `Value::Ref` the §21.1 planner walks, so a `restrict`
/// target stays undeletable. If the object decode stranded a `Value::Struct`
/// pseudo-member, the delete would be admitted (a dangling ref).
#[test]
fn composite_object_operand_add_is_planner_visible() {
    let mut e = load("d-comp-add", &composite_pkg("restrict"));
    committed(call(&mut e, &CallRequest::new("add_account").arg("org", text("acme")).arg("user", text("ann"))));
    committed(call(&mut e, &CallRequest::new("add_doc").arg("id", text("d1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer_obj").arg("id", text("d1")).arg("org", text("acme")).arg("user", text("ann"))));

    // The stored member must be the composite Ref, exactly as the add-by-Ref form.
    assert_eq!(reviewers(&e), vec![composite_ref("acme", "ann")], "object operand decodes to a composite Ref");

    let head = e.head();
    let outcome = call(&mut e, &CallRequest::new("delete_account").arg("org", text("acme")).arg("user", text("ann")));
    assert_eq!(
        outcome.rejection().map(|r| r.reason()),
        Some(RejectionReason::Restricted),
        "the object-operand-added membership must restrict deletion (§21.1); got {outcome:?}"
    );
    assert_eq!(e.head(), head, "a blocked delete leaves no commit");
}

/// §5.5/§5.6: removing a present composite membership by a bare OBJECT operand
/// must remove it (decode to the same stored Ref), unblocking a `restrict`
/// delete. A remove that no-ops would leave the target permanently undeletable.
#[test]
fn composite_object_operand_remove_unblocks_delete() {
    let mut e = load("d-comp-remove", &composite_pkg("restrict"));
    committed(call(&mut e, &CallRequest::new("add_account").arg("org", text("acme")).arg("user", text("ann"))));
    committed(call(&mut e, &CallRequest::new("add_doc").arg("id", text("d1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer_obj").arg("id", text("d1")).arg("org", text("acme")).arg("user", text("ann"))));
    committed(call(&mut e, &CallRequest::new("drop_reviewer_obj").arg("id", text("d1")).arg("org", text("acme")).arg("user", text("ann"))));
    assert!(reviewers(&e).is_empty(), "the object-operand remove must clear the membership");

    committed(call(&mut e, &CallRequest::new("delete_account").arg("org", text("acme")).arg("user", text("ann"))));
    assert_eq!(count(&e, "accounts_view"), 0, "the target is deletable once the membership is removed (§21.1)");
}

/// §5.5: adding an existing composite membership by a bare OBJECT operand leaves
/// the set unchanged — no duplicate application-visible identity.
#[test]
fn composite_object_operand_add_existing_collapses() {
    let mut e = load("d-comp-dup", &composite_pkg("cascade"));
    committed(call(&mut e, &CallRequest::new("add_account").arg("org", text("acme")).arg("user", text("ann"))));
    committed(call(&mut e, &CallRequest::new("add_doc").arg("id", text("d1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer_obj").arg("id", text("d1")).arg("org", text("acme")).arg("user", text("ann"))));
    // §5.5: re-adding the existing member leaves the set unchanged, so the second
    // add is a no-op (`Unchanged`), not a commit — and never a duplicate.
    let again = call(&mut e, &CallRequest::new("add_reviewer_obj").arg("id", text("d1")).arg("org", text("acme")).arg("user", text("ann")));
    assert!(matches!(again, CallOutcome::Unchanged { .. }), "add-existing is unchanged, got {again:?}");
    assert_eq!(reviewers(&e), vec![composite_ref("acme", "ann")], "add-existing must collapse, not duplicate (§5.5)");
}

// -- Struct key: operand is the bare struct key value ----------------------

fn struct_pkg(on_delete: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1,
  "$app": "t.setstructdecode@1.0.0",
  "$model": {{
    "accounts": {{ "$key": "acct", "acct": {{ "org": "text", "user": "text" }} }},
    "docs": {{ "$key": "id", "id": "text",
      "reviewers": {{ "$set": {{ "$ref": "/accounts", "$on_delete": "{on_delete}" }} }} }},
    "docs_view": {{ "$view": ".docs {{ id, reviewers, $sort: [id] }}" }},
    "accounts_view": {{ "$view": ".accounts {{ acct, $sort: [acct] }}" }},
    "$mut": {{
      "add_account": ".accounts + {{ acct: {{ org: @org, user: @user }} }}",
      "add_doc": ".docs + {{ id: @id }}",
      "add_reviewer_key": ".docs[@id].reviewers + @acct",
      "drop_reviewer_key": ".docs[@id].reviewers - @acct",
      "delete_account": ".accounts - @acct"
    }}
  }}
}}"#
    )
}

/// §5.6/§21.1: adding a struct-keyed reviewer through the bare STRUCT key value
/// (not a pre-built `Ref`) must decode to `Ref::scalar(Struct)` and be
/// planner-visible, so a `restrict` target stays undeletable.
#[test]
fn struct_key_operand_add_is_planner_visible() {
    let mut e = load("d-struct-add", &struct_pkg("restrict"));
    committed(call(&mut e, &CallRequest::new("add_account").arg("org", text("acme")).arg("user", text("ann"))));
    committed(call(&mut e, &CallRequest::new("add_doc").arg("id", text("d1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer_key").arg("id", text("d1")).arg("acct", struct_key("acme", "ann"))));

    assert_eq!(reviewers(&e), vec![struct_ref("acme", "ann")], "struct key operand decodes to a struct-keyed Ref");

    let head = e.head();
    let outcome = call(&mut e, &CallRequest::new("delete_account").arg("acct", struct_key("acme", "ann")));
    assert_eq!(
        outcome.rejection().map(|r| r.reason()),
        Some(RejectionReason::Restricted),
        "the struct-key-added membership must restrict deletion (§21.1); got {outcome:?}"
    );
    assert_eq!(e.head(), head, "a blocked delete leaves no commit");
}

/// §5.5/§5.6: removing a present struct-keyed membership by its bare struct key
/// value must remove it, unblocking a `restrict` delete.
#[test]
fn struct_key_operand_remove_unblocks_delete() {
    let mut e = load("d-struct-remove", &struct_pkg("restrict"));
    committed(call(&mut e, &CallRequest::new("add_account").arg("org", text("acme")).arg("user", text("ann"))));
    committed(call(&mut e, &CallRequest::new("add_doc").arg("id", text("d1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer_key").arg("id", text("d1")).arg("acct", struct_key("acme", "ann"))));
    committed(call(&mut e, &CallRequest::new("drop_reviewer_key").arg("id", text("d1")).arg("acct", struct_key("acme", "ann"))));
    assert!(reviewers(&e).is_empty(), "the struct-key remove must clear the membership");

    committed(call(&mut e, &CallRequest::new("delete_account").arg("acct", struct_key("acme", "ann"))));
    assert_eq!(count(&e, "accounts_view"), 0, "the target is deletable once the membership is removed (§21.1)");
}
