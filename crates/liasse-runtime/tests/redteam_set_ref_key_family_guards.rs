#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team convergence guards for the just-landed §5.5/§5.6/§21.1 `$set` of
//! `$ref` governance across the key family. The landed `set_ref_on_delete`
//! tests cover only a SCALAR-keyed target; these lock in the combinations that
//! were previously UNTESTED and that the composite/struct-key rework touches:
//!
//! - a set member that is a COMPOSITE-keyed ref (`Ref::composite`, B.4/A.9);
//! - a set member that is a STRUCT-keyed ref (A.8, `Ref::scalar(Struct)`);
//! - a set-of-ref held on a COMPOSITE-keyed CONTAINING row (the DropMember
//!   effect must re-address that row by its positional `Value::Composite`);
//! - a set-member `cascade` whose target is removed through a MULTI-HOP cascade;
//! - a `= patch` set-member whose patch does NOT drop the member: §22.1 forbids
//!   the resulting dangling membership, so finalize MUST reject the transition.
//!
//! Every expectation is re-derived from the cited spec text, not the engine.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, RejectionReason, Value};
use liasse_store::MemoryStore;
use liasse_value::{Ref, RefKey, Struct, Text};
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

// -- 1. set member is a COMPOSITE-keyed ref -------------------------------

fn composite_ref(org: &str, user: &str) -> Value {
    Value::Ref(Ref::composite(vec![text(org), text(user)]))
}

fn composite_pkg(on_delete: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1,
  "$app": "t.setcompref@1.0.0",
  "$model": {{
    "accounts": {{ "$key": ["org", "user"], "org": "text", "user": "text" }},
    "docs": {{ "$key": "id", "id": "text",
      "reviewers": {{ "$set": {{ "$ref": "/accounts", "$on_delete": "{on_delete}" }} }} }},
    "docs_view": {{ "$view": ".docs {{ id, reviewers, $sort: [id] }}" }},
    "accounts_view": {{ "$view": ".accounts {{ org, user, $sort: [org, user] }}" }},
    "$mut": {{
      "add_account": ".accounts + {{ org: @org, user: @user }}",
      "add_doc": ".docs + {{ id: @id }}",
      "add_reviewer": ".docs[@id].reviewers + @acct",
      "delete_account": ".accounts - {{ org: @org, user: @user }}"
    }}
  }}
}}"#
    )
}

#[test]
fn cascade_drops_composite_keyed_set_member() {
    let mut e = load("g-comp-cascade", &composite_pkg("cascade"));
    committed(call(&mut e, &CallRequest::new("add_account").arg("org", text("acme")).arg("user", text("ann"))));
    committed(call(&mut e, &CallRequest::new("add_account").arg("org", text("acme")).arg("user", text("bob"))));
    committed(call(&mut e, &CallRequest::new("add_doc").arg("id", text("d1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer").arg("id", text("d1")).arg("acct", composite_ref("acme", "ann"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer").arg("id", text("d1")).arg("acct", composite_ref("acme", "bob"))));

    committed(call(&mut e, &CallRequest::new("delete_account").arg("org", text("acme")).arg("user", text("bob"))));
    assert_eq!(count(&e, "accounts_view"), 1, "only [acme,bob] deleted");
    assert_eq!(
        reviewers(&e),
        vec![composite_ref("acme", "ann")],
        "§22.1: the composite [acme,bob] member is dropped, [acme,ann] remains"
    );
}

#[test]
fn restrict_blocks_delete_of_composite_keyed_target() {
    let mut e = load("g-comp-restrict", &composite_pkg("restrict"));
    committed(call(&mut e, &CallRequest::new("add_account").arg("org", text("acme")).arg("user", text("ann"))));
    committed(call(&mut e, &CallRequest::new("add_doc").arg("id", text("d1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer").arg("id", text("d1")).arg("acct", composite_ref("acme", "ann"))));
    let head = e.head().unwrap();
    let outcome = call(&mut e, &CallRequest::new("delete_account").arg("org", text("acme")).arg("user", text("ann")));
    assert_eq!(outcome.rejection().map(|r| r.reason()), Some(RejectionReason::Restricted));
    assert_eq!(e.head().unwrap(), head, "a blocked delete leaves no commit");
}

// -- 2. set member is a STRUCT-keyed ref (A.8) ----------------------------

fn struct_key(org: &str, user: &str) -> Value {
    Value::Struct(Struct::new([(Text::new("org"), text(org)), (Text::new("user"), text(user))]))
}

fn struct_ref(org: &str, user: &str) -> Value {
    Value::Ref(Ref::scalar(struct_key(org, user)))
}

fn struct_pkg(on_delete: &str) -> String {
    format!(
        r#"{{
  "$liasse": 1,
  "$app": "t.setstructref@1.0.0",
  "$model": {{
    "accounts": {{ "$key": "acct", "acct": {{ "org": "text", "user": "text" }} }},
    "docs": {{ "$key": "id", "id": "text",
      "reviewers": {{ "$set": {{ "$ref": "/accounts", "$on_delete": "{on_delete}" }} }} }},
    "docs_view": {{ "$view": ".docs {{ id, reviewers, $sort: [id] }}" }},
    "accounts_view": {{ "$view": ".accounts {{ acct, $sort: [acct] }}" }},
    "$mut": {{
      "add_account": ".accounts + {{ acct: {{ org: @org, user: @user }} }}",
      "add_doc": ".docs + {{ id: @id }}",
      "add_reviewer": ".docs[@id].reviewers + @acct",
      "delete_account": ".accounts - @acct"
    }}
  }}
}}"#
    )
}

#[test]
fn cascade_drops_struct_keyed_set_member() {
    let mut e = load("g-struct-cascade", &struct_pkg("cascade"));
    committed(call(&mut e, &CallRequest::new("add_account").arg("org", text("acme")).arg("user", text("ann"))));
    committed(call(&mut e, &CallRequest::new("add_account").arg("org", text("acme")).arg("user", text("bob"))));
    committed(call(&mut e, &CallRequest::new("add_doc").arg("id", text("d1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer").arg("id", text("d1")).arg("acct", struct_ref("acme", "ann"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer").arg("id", text("d1")).arg("acct", struct_ref("acme", "bob"))));

    committed(call(&mut e, &CallRequest::new("delete_account").arg("acct", struct_key("acme", "bob"))));
    assert_eq!(count(&e, "accounts_view"), 1, "only [acme,bob] deleted");
    assert_eq!(
        reviewers(&e),
        vec![struct_ref("acme", "ann")],
        "§22.1: the struct-keyed [acme,bob] member is dropped, [acme,ann] remains"
    );
}

#[test]
fn restrict_blocks_delete_of_struct_keyed_target() {
    let mut e = load("g-struct-restrict", &struct_pkg("restrict"));
    committed(call(&mut e, &CallRequest::new("add_account").arg("org", text("acme")).arg("user", text("ann"))));
    committed(call(&mut e, &CallRequest::new("add_doc").arg("id", text("d1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer").arg("id", text("d1")).arg("acct", struct_ref("acme", "ann"))));
    let head = e.head().unwrap();
    let outcome = call(&mut e, &CallRequest::new("delete_account").arg("acct", struct_key("acme", "ann")));
    assert_eq!(outcome.rejection().map(|r| r.reason()), Some(RejectionReason::Restricted));
    assert_eq!(e.head().unwrap(), head, "a blocked delete leaves no commit");
}

// -- 3. set-of-ref on a COMPOSITE-keyed CONTAINING row --------------------

fn scalar_ref(id: &str) -> Value {
    Value::Ref(Ref::scalar(text(id)))
}

const CONTAINER_PKG: &str = r#"{
  "$liasse": 1,
  "$app": "t.setcompcontainer@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text" },
    "docs": { "$key": ["space", "slug"], "space": "text", "slug": "text",
      "reviewers": { "$set": { "$ref": "/accounts", "$on_delete": "cascade" } } },
    "docs_view": { "$view": ".docs { space, slug, reviewers, $sort: [space, slug] }" },
    "$mut": {
      "add_account": ".accounts + { id: @id }",
      "add_doc": ".docs + { space: @space, slug: @slug }",
      "add_reviewer": ".docs[{ space: @space, slug: @slug }].reviewers + @acct",
      "delete_account": ".accounts - @id"
    }
  }
}"#;

#[test]
fn cascade_drops_member_on_composite_keyed_containing_row() {
    let mut e = load("g-comp-container", CONTAINER_PKG);
    committed(call(&mut e, &CallRequest::new("add_account").arg("id", text("a1"))));
    committed(call(&mut e, &CallRequest::new("add_account").arg("id", text("a2"))));
    committed(call(&mut e, &CallRequest::new("add_doc").arg("space", text("eng")).arg("slug", text("readme"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer").arg("space", text("eng")).arg("slug", text("readme")).arg("acct", scalar_ref("a1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer").arg("space", text("eng")).arg("slug", text("readme")).arg("acct", scalar_ref("a2"))));

    committed(call(&mut e, &CallRequest::new("delete_account").arg("id", text("a1"))));
    assert_eq!(
        reviewers(&e),
        vec![scalar_ref("a2")],
        "§22.1: DropMember re-addresses the composite-keyed doc row; a1 dropped, a2 remains"
    );
}

// -- 4. multi-hop DECIDED cascade set-member ------------------------------

const MULTIHOP_PKG: &str = r#"{
  "$liasse": 1,
  "$app": "t.multihopdecided@1.0.0",
  "$model": {
    "orgs": { "$key": "id", "id": "text" },
    "teams": { "$key": "id", "id": "text", "org": { "$ref": "/orgs", "$on_delete": "cascade" } },
    "docs": { "$key": "id", "id": "text",
      "reviewers": { "$set": { "$ref": "/teams", "$on_delete": "cascade" } } },
    "docs_view": { "$view": ".docs { id, reviewers, $sort: [id] }" },
    "$mut": {
      "add_org": ".orgs + { id: @id }",
      "add_team": ".teams + { id: @id, org: @org }",
      "add_doc": ".docs + { id: @id }",
      "add_reviewer": ".docs[@id].reviewers + @team",
      "delete_org": ".orgs - @id"
    }
  }
}"#;

#[test]
fn multihop_decided_cascade_drops_set_member_cleanly() {
    // §21.1: deleting an org cascades the team, whose set-member (a DECIDED
    // cascade) is dropped through the multi-hop chain, leaving no dangling.
    let mut e = load("g-multihop", MULTIHOP_PKG);
    committed(call(&mut e, &CallRequest::new("add_org").arg("id", text("o1"))));
    committed(call(&mut e, &CallRequest::new("add_org").arg("id", text("o2"))));
    committed(call(&mut e, &CallRequest::new("add_team").arg("id", text("t1")).arg("org", scalar_ref("o1"))));
    committed(call(&mut e, &CallRequest::new("add_team").arg("id", text("t2")).arg("org", scalar_ref("o2"))));
    committed(call(&mut e, &CallRequest::new("add_doc").arg("id", text("d1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer").arg("id", text("d1")).arg("team", scalar_ref("t1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer").arg("id", text("d1")).arg("team", scalar_ref("t2"))));

    committed(call(&mut e, &CallRequest::new("delete_org").arg("id", text("o1"))));
    assert_eq!(
        reviewers(&e),
        vec![scalar_ref("t2")],
        "§22.1: multi-hop cascade drops the t1 member cleanly, t2 remains"
    );
}

// -- 5. `= patch` set-member dangling residual is rejected by finalize ----

const PATCH_PKG: &str = r#"{
  "$liasse": 1,
  "$app": "t.setpatchmember@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text" },
    "docs": { "$key": "id", "id": "text", "flagged": "bool = false",
      "reviewers": { "$set": { "$ref": "/accounts", "$on_delete": "= { flagged: true }" } } },
    "docs_view": { "$view": ".docs { id, flagged, reviewers, $sort: [id] }" },
    "accounts_view": { "$view": ".accounts { id, $sort: [id] }" },
    "$mut": {
      "add_account": ".accounts + { id: @id }",
      "add_doc": ".docs + { id: @id }",
      "add_reviewer": ".docs[@id].reviewers + @acct",
      "delete_account": ".accounts - @id"
    }
  }
}"#;

#[test]
fn patch_set_member_dangling_residual_is_rejected_by_finalize() {
    // §5.6: `= patch` on a set member patches the containing row and does NOT drop
    // the member. Deleting the target leaves a member pointing at a removed row,
    // which §22.1 forbids, so finalize MUST reject the whole transition — the
    // documented set-member `= patch` seam is backstopped for a direct delete.
    let mut e = load("g-patch-member", PATCH_PKG);
    committed(call(&mut e, &CallRequest::new("add_account").arg("id", text("a1"))));
    committed(call(&mut e, &CallRequest::new("add_doc").arg("id", text("d1"))));
    committed(call(&mut e, &CallRequest::new("add_reviewer").arg("id", text("d1")).arg("acct", scalar_ref("a1"))));
    let head = e.head().unwrap();

    let outcome = call(&mut e, &CallRequest::new("delete_account").arg("id", text("a1")));
    assert_eq!(
        outcome.rejection().map(|r| r.reason()),
        Some(RejectionReason::DanglingRef),
        "a `= patch` that leaves a dangling set member must be rejected (§22.1); got {outcome:?}"
    );
    assert_eq!(e.head().unwrap(), head, "the rejected transition leaves no commit");
    // The rolled-back state is intact: a1 still live, still a member.
    assert_eq!(count(&e, "accounts_view"), 1, "a1 survives the rejected delete");
    let dangling = reviewers(&e).iter().any(|m| matches!(m, Value::Ref(r) if matches!(r.key(), RefKey::Scalar(k) if **k == text("a1"))));
    assert!(dangling, "a1 member is still present after rollback");
}
