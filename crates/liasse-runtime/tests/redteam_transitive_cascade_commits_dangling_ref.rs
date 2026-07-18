#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! FINDING (red team): a `collection - key` delete whose cascade removes a row
//! that is the target of an inbound reference with an UNDECIDED `$on_delete`
//! commits a §22.1 reference-validity violation — a live reference to a removed
//! row — instead of rejecting the transition.
//!
//! Setup: `accounts` is directly deletable (`.accounts - @id`). `teams.owner`
//! refs `/accounts` with `cascade`, so deleting an account cascades the team
//! away — i.e. `teams` IS deletable (transitively, §21.1 lists "cascade-induced
//! row removal" among the operations that make a target deletable). A third
//! relation references `/teams` and leaves `$on_delete` undecided.
//!
//! §21.1 requires EITHER outcome:
//!   (a) the package is REJECTED at load — `teams` is deletable, so the undecided
//!       inbound ref to `/teams` is illegal ("The checker computes possible
//!       deletion transitively"); OR
//!   (b) if admitted, the runtime NEVER commits a state that violates §22.1
//!       reference validity ("State constraints hold in every committed state").
//!
//! The engine does NEITHER: it admits the package (the §21.1 governance gate,
//! `liasse-model/src/delete.rs` `deletable_targets`, scans only directly-authored
//! delete operators — an acknowledged CORE seam, delete.rs:22-28) AND then, at
//! delete time, COMMITS the dangling reference, because the runtime finalize
//! integrity pass (`liasse-runtime/src/rules.rs` `finalize`) re-validates only
//! `touched` rows and the cascade never touches the third relation's row (its
//! undecided inbound edge is skipped by the planner, `cascade.rs` `resolve_policy`
//! / `resolve_member_policy` -> `None`). The committed dangling ref is reproduced
//! for BOTH a scalar ref and a `$set`-of-`$ref` member, so it is the general
//! cascade-transitive seam, not specific to the newly-landed set-of-ref feature.
//!
//! Expected: no committed reference points at a removed row. Actual: it does.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::{Ref, RefKey, Text};
use support::{generator, store};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn scalar_ref(id: &str) -> Value {
    Value::Ref(Ref::scalar(text(id)))
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut g = generator();
    engine.call(request, &mut g).expect("call")
}

fn committed(outcome: CallOutcome) {
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "expected a commit, got {outcome:?}");
}

fn live_team_ids(engine: &Engine<MemoryStore>) -> Vec<Value> {
    engine
        .view_at_head("teams_view")
        .expect("view")
        .expect("declared")
        .rows()
        .iter()
        .map(|r| r.field("id").expect("id").clone())
        .collect()
}

fn is_dangling(key: &Value, live: &[Value]) -> bool {
    match key {
        Value::Ref(r) => match r.key() {
            RefKey::Scalar(k) => !live.contains(k),
            RefKey::Composite(_) => false,
        },
        _ => false,
    }
}

// -- set-of-ref member with an undecided policy to a transitively-deletable target

const SET_PKG: &str = r#"{
  "$liasse": 1,
  "$app": "t.transitivecascadeset@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text" },
    "teams": { "$key": "id", "id": "text", "owner": { "$ref": "/accounts", "$on_delete": "cascade" } },
    "docs": { "$key": "id", "id": "text", "reviewers": { "$set": { "$ref": "/teams" } } },
    "docs_view": { "$view": ".docs { id, reviewers, $sort: [id] }" },
    "teams_view": { "$view": ".teams { id, $sort: [id] }" },
    "$mut": {
      "add_account": ".accounts + { id: @id }",
      "add_team": ".teams + { id: @id, owner: @owner }",
      "add_doc": ".docs + { id: @id }",
      "add_reviewer": ".docs[@id].reviewers + @team",
      "delete_account": ".accounts - @id"
    }
  }
}"#;

#[test]
fn set_of_ref_transitive_cascade_must_not_commit_dangling_member() {
    let load = {
        let mut g = generator();
        Engine::load(store("transitive-cascade-set"), SET_PKG, &mut g)
    };
    // (a) spec-correct: §21.1 load rejection of the undecided inbound ref.
    let mut engine = match load {
        Err(_) => return,
        Ok(engine) => engine,
    };
    // (b) admitted -> the runtime must still honor §22.1.
    committed(call(&mut engine, &CallRequest::new("add_account").arg("id", text("a1"))));
    committed(call(&mut engine, &CallRequest::new("add_team").arg("id", text("t1")).arg("owner", scalar_ref("a1"))));
    committed(call(&mut engine, &CallRequest::new("add_doc").arg("id", text("d1"))));
    committed(call(&mut engine, &CallRequest::new("add_reviewer").arg("id", text("d1")).arg("team", scalar_ref("t1"))));

    // Deleting a1 cascades team t1 out of live state.
    let outcome = call(&mut engine, &CallRequest::new("delete_account").arg("id", text("a1")));

    let live = live_team_ids(&engine);
    let members = match engine.view_at_head("docs_view").expect("view").expect("declared").rows()[0].field("reviewers") {
        Some(Value::Set(m)) => m.iter().cloned().collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let dangling: Vec<&Value> = members.iter().filter(|m| is_dangling(m, &live)).collect();
    assert!(
        dangling.is_empty(),
        "§22.1 violated: docs.reviewers committed member(s) {dangling:?} pointing at a team absent from live state {live:?} \
         (delete outcome {outcome:?})"
    );
}

// -- scalar ref with an undecided policy: the same violation, showing generality

const SCALAR_PKG: &str = r#"{
  "$liasse": 1,
  "$app": "t.transitivecascadescalar@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text" },
    "teams": { "$key": "id", "id": "text", "owner": { "$ref": "/accounts", "$on_delete": "cascade" } },
    "docs": { "$key": "id", "id": "text", "lead": { "$ref": "/teams" } },
    "docs_view": { "$view": ".docs { id, lead, $sort: [id] }" },
    "teams_view": { "$view": ".teams { id, $sort: [id] }" },
    "$mut": {
      "add_account": ".accounts + { id: @id }",
      "add_team": ".teams + { id: @id, owner: @owner }",
      "add_doc": ".docs + { id: @id, lead: @lead }",
      "delete_account": ".accounts - @id"
    }
  }
}"#;

#[test]
fn scalar_ref_transitive_cascade_must_not_commit_dangling_ref() {
    let load = {
        let mut g = generator();
        Engine::load(store("transitive-cascade-scalar"), SCALAR_PKG, &mut g)
    };
    let mut engine = match load {
        Err(_) => return,
        Ok(engine) => engine,
    };
    committed(call(&mut engine, &CallRequest::new("add_account").arg("id", text("a1"))));
    committed(call(&mut engine, &CallRequest::new("add_team").arg("id", text("t1")).arg("owner", scalar_ref("a1"))));
    committed(call(&mut engine, &CallRequest::new("add_doc").arg("id", text("d1")).arg("lead", scalar_ref("t1"))));

    let outcome = call(&mut engine, &CallRequest::new("delete_account").arg("id", text("a1")));

    let live = live_team_ids(&engine);
    let lead = engine.view_at_head("docs_view").expect("view").expect("declared").rows()[0].field("lead").cloned();
    let dangling = lead.as_ref().is_some_and(|v| is_dangling(v, &live));
    assert!(
        !dangling,
        "§22.1 violated: docs.lead committed {lead:?} pointing at a team absent from live state {live:?} \
         (delete outcome {outcome:?})"
    );
}
