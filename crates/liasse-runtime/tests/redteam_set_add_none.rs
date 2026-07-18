#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM — SPEC-ISSUES #29 / §5.5 / Annex A.1: **adding `none` to a set is a
//! no-op**.
//!
//! SPEC.md §5.5 (line 483): "Adding `none` to a set is a no-op that leaves the set
//! unchanged, mirroring set membership below." Annex A.1 (line 4355): "set element:
//! `none` is **not a member**. `none` is never a valid set element, and adding
//! `none` to a set is a no-op that yields the same set." A set therefore NEVER
//! contains a `none` member, and `.tags + none` MUST leave the set byte-for-byte
//! unchanged and produce no state change.
//!
//! These are pure `liasse-runtime` reproductions (no harness in the loop): the
//! literal `none` is written directly in the mutation body, `@id` is bound as a
//! genuine argument, and the resulting set membership is read straight off the
//! runtime's own `view_at_head`. Every expectation is deducible from SPEC.md
//! alone — the set the store must hold is `{a, b}` (or `{}`), derived from the
//! spec's "no-op" rule, never echoed from the runtime.
//!
//! BUG (reproduced): the runtime instead **inserts `Value::None` as a set member**
//! and reports the request as a committed state change. Root cause:
//! `crates/liasse-runtime/src/interp.rs` `set_mutate` (the `members.insert(member)`
//! at lines ~1111-1112) inserts every incoming operand unconditionally, with no
//! guard filtering out `Value::None` — so the §5.5/A.1 "add-none is a no-op" rule
//! is never enforced. Both a scalar-element set and a `$ref`-element set are
//! affected identically.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::{Text, Value as V};
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut generator = generator();
    engine.call(request, &mut generator).expect("call runs")
}

fn set_members(engine: &Engine<MemoryStore>, view: &str, field: &str) -> Vec<Value> {
    let result = engine.view_at_head(view).expect("view").expect("declared view");
    match result.rows()[0].field(field) {
        Some(V::Set(members)) => members.iter().cloned().collect(),
        other => panic!("field `{field}` is not a set: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Scalar-element set: `.tags + none` must be a no-op (§5.5 / A.1).
// ---------------------------------------------------------------------------

const SCALAR_SET: &str = r#"{
  "$liasse": 1,
  "$app": "redteam.setnone.scalar@1.0.0",
  "$model": {
    "docs": { "$key": "id", "id": "text", "tags": { "$set": "text" } },
    "docs_view": { "$view": ".docs { id, tags }" },
    "$mut": {
      "add": ".docs + { id: @id, tags: @tags }",
      "add_none": ".docs[@id].tags + none",
      "add_existing": ".docs[@id].tags + 'a'",
      "remove_none": ".docs[@id].tags - none"
    }
  }
}"#;

#[test]
fn scalar_set_add_none_must_be_a_noop_but_inserts_none_member() {
    let mut engine = load("setnone-scalar", SCALAR_SET);
    let seed = Value::Set([text("a"), text("b")].into_iter().collect());
    assert!(matches!(
        call(&mut engine, &CallRequest::new("add").arg("id", text("d1")).arg("tags", seed)),
        CallOutcome::Committed { .. }
    ));

    // PASSING CONTROL 1: adding an already-present member is a no-op that produces
    // no commit (§5.5 "adding an existing member leaves the set unchanged").
    let existing = call(&mut engine, &CallRequest::new("add_existing").arg("id", text("d1")));
    assert!(existing.committed_at().is_none(), "add-existing must be Unchanged (§5.5)");
    assert_eq!(set_members(&engine, "docs_view", "tags"), vec![text("a"), text("b")]);

    // PASSING CONTROL 2: removing an absent member (`none` is never a member) is a
    // no-op (§5.5 "removing an absent member leaves it unchanged").
    let removed = call(&mut engine, &CallRequest::new("remove_none").arg("id", text("d1")));
    assert!(removed.committed_at().is_none(), "remove-none must be Unchanged (§5.5)");
    assert_eq!(set_members(&engine, "docs_view", "tags"), vec![text("a"), text("b")]);

    // THE BUG: `.tags + none` must be a no-op — the set stays exactly `{a, b}` and no
    // commit is produced (§5.5 line 483 / A.1 line 4355). The runtime instead
    // inserts `Value::None` and commits the change.
    let outcome = call(&mut engine, &CallRequest::new("add_none").arg("id", text("d1")));
    assert!(
        outcome.committed_at().is_none(),
        "SPEC §5.5/A.1: adding `none` to a set is a no-op, so it MUST NOT commit a change; \
         got a commit at {:?}",
        outcome.committed_at()
    );
    let members = set_members(&engine, "docs_view", "tags");
    assert!(
        !members.contains(&Value::None),
        "SPEC A.1 line 4355: `none` is never a set member, but the stored set contains it: {members:?}"
    );
    assert_eq!(
        members,
        vec![text("a"), text("b")],
        "SPEC §5.5 line 483: `.tags + none` MUST leave the set unchanged as {{a, b}}"
    );
}

// ---------------------------------------------------------------------------
// `$ref`-element set: the same no-op rule holds (§5.5 governs every set).
// ---------------------------------------------------------------------------

const REF_SET: &str = r#"{
  "$liasse": 1,
  "$app": "redteam.setnone.ref@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text" },
    "docs": { "$key": "id", "id": "text", "deps": { "$set": { "$ref": "/accounts" } } },
    "docs_view": { "$view": ".docs { id, deps }" },
    "$mut": {
      "add": ".docs + { id: @id }",
      "add_none": ".docs[@id].deps + none"
    }
  }
}"#;

#[test]
fn ref_set_add_none_must_be_a_noop_but_inserts_none_member() {
    let mut engine = load("setnone-ref", REF_SET);
    assert!(matches!(
        call(&mut engine, &CallRequest::new("add").arg("id", text("d1"))),
        CallOutcome::Committed { .. }
    ));
    // A freshly-created row starts with an empty set (§5.5 "omitted child set starts empty").
    assert_eq!(set_members(&engine, "docs_view", "deps"), Vec::<Value>::new());

    // THE BUG: `.deps + none` on a `$ref`-set must also be a no-op; instead the
    // runtime inserts `Value::None` (an invalid, planner-invisible pseudo-member of
    // a ref set) and commits it.
    let outcome = call(&mut engine, &CallRequest::new("add_none").arg("id", text("d1")));
    assert!(
        outcome.committed_at().is_none(),
        "SPEC §5.5/A.1: adding `none` to a ref set is a no-op; it MUST NOT commit"
    );
    let members = set_members(&engine, "docs_view", "deps");
    assert!(
        !members.contains(&Value::None),
        "SPEC A.1 line 4355: a ref set must never hold a `none` member: {members:?}"
    );
    assert!(members.is_empty(), "the ref set MUST stay empty after `.deps + none`");
}
