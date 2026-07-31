#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.5 set-field mutations (`.tags + m` union, `.tags - m` difference) and
//! §8.9 no-change completion, driven through a row mutation. Each expectation is
//! re-derived from §8.5/§8.9: adding an existing member or removing an absent one
//! succeeds without changing state; a genuine change commits. Set read order is
//! the element type's canonical order (§5.5, Annex B.1).

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::{Text, Value as V};
use support::{generator, load};

const DOCS: &str = r#"{
  "$liasse": 1,
  "$app": "example.setfields@1.0.0",
  "$model": {
    "docs": {
      "$key": "id",
      "id": "text",
      "tags": { "$set": "text" },
      "$mut": { "tag": ".tags + @tag", "untag": ".tags - @tag" }
    },
    "docs_view": { "$view": ".docs { id, tags }" },
    "$mut": { "add_doc": ".docs + { id: @id }" }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) -> CallOutcome {
    let mut generator = generator();
    engine.call(request, &mut generator).expect("call")
}

fn tags(engine: &Engine<MemoryStore>) -> Vec<Value> {
    let view = engine.view_at_head("docs_view").expect("view").expect("declared");
    match view.rows()[0].field("tags") {
        Some(V::Set(members)) => members.iter().cloned().collect(),
        other => panic!("tags is a set, got {other:?}"),
    }
}

#[test]
fn omitted_set_starts_empty() {
    // §5.5: when a row is created, an omitted set starts empty — an empty set,
    // not an absent optional. It projects as an empty set (wire `[]`) rather
    // than a missing member.
    let mut engine = load("setfields", DOCS);
    assert!(matches!(
        call(&mut engine, &CallRequest::new("add_doc").arg("id", text("d1"))),
        CallOutcome::Committed { .. }
    ));
    let view = engine.view_at_head("docs_view").expect("view").expect("declared");
    match view.rows()[0].field("tags") {
        Some(V::Set(members)) => assert!(members.is_empty(), "an omitted set starts empty"),
        other => panic!("an omitted set is an empty set, not {other:?}"),
    }
}

#[test]
fn set_add_and_remove_apply_and_noop() {
    let mut engine = load("setfields", DOCS);
    assert!(matches!(
        call(&mut engine, &CallRequest::new("add_doc").arg("id", text("d1"))),
        CallOutcome::Committed { .. }
    ));

    // First tag: a genuine addition commits.
    assert!(matches!(
        call(&mut engine, &CallRequest::new("tag").receiver(text("d1")).arg("tag", text("a"))),
        CallOutcome::Committed { .. }
    ));
    // §8.5/§8.9: re-adding an existing member changes nothing → unchanged.
    assert!(matches!(
        call(&mut engine, &CallRequest::new("tag").receiver(text("d1")).arg("tag", text("a"))),
        CallOutcome::Unchanged { .. }
    ));
    // Removing an absent member changes nothing → unchanged.
    assert!(matches!(
        call(&mut engine, &CallRequest::new("untag").receiver(text("d1")).arg("tag", text("z"))),
        CallOutcome::Unchanged { .. }
    ));
    // A second distinct member commits; canonical order is "a" < "b" (B.1).
    assert!(matches!(
        call(&mut engine, &CallRequest::new("tag").receiver(text("d1")).arg("tag", text("b"))),
        CallOutcome::Committed { .. }
    ));
    assert_eq!(tags(&engine), vec![text("a"), text("b")]);

    // Removing a present member commits and drops it from the set.
    assert!(matches!(
        call(&mut engine, &CallRequest::new("untag").receiver(text("d1")).arg("tag", text("a"))),
        CallOutcome::Committed { .. }
    ));
    assert_eq!(tags(&engine), vec![text("b")]);
}

/// §8.5 union/difference at a §8.2 ROOT-SINGLETON `$set` member. §5.5 makes a root
/// `$set` durable state at its own address and §8.5 scopes `+`/`-` to a set field,
/// not to keyed collections — so `.flags + @t` on the package root must add a
/// member and `.flags - @t` must remove one.
///
/// Before the fix the root arm resolved no row target at all: the statement staged
/// NOTHING while the call still reported a commit, so a declared root-set mutation
/// silently did nothing. The no-change completions below (§8.9) also prove the
/// mutation is genuinely reaching the set rather than reporting `Committed` blindly.
const ROOT_FLAGS: &str = r#"{
  "$liasse": 1,
  "$app": "example.rootset@1.0.0",
  "$model": {
    "flags": { "$set": "text" },
    "root_view": { "$view": ". { flags }" },
    "$mut": { "flag": ".flags + @t", "unflag": ".flags - @t" }
  }
}"#;

/// §5.5/§8.2: the §8.2 root's omitted CONTAINERS start empty, exactly as an omitted
/// container inside a created row or static struct does. §8.2 makes the package root
/// durable state "at every moment of an instance's life ... whether or not any member
/// has been written", so there is no unwritten-member exemption at the root: a `$set`
/// no write has touched holds the empty set, a `map`-valued member the empty map, and
/// a `$set` inside a root static struct the empty set — §5.5 gives its rule to "a
/// containing row or struct" and §5.1 exempts "a set or map-valued field" from the
/// required-population rule for exactly this reason.
///
/// Before the fix every one of these read `none` (the member was dropped from the
/// projection entirely), and the root `map` member could not even be declared: genesis
/// refused the package with "required field `settings` is unpopulated".
const ROOT_CONTAINERS: &str = r#"{
  "$liasse": 1,
  "$app": "example.rootcontainers@1.0.0",
  "$model": {
    "flags": { "$set": "text" },
    "settings": "{ $key: text, $value: text }",
    "cfg": { "labels": { "$set": "text" }, "name": "text" },
    "root_view": { "$view": ". { flags, settings, labels: .cfg.labels }" }
  },
  "$data": { "cfg": { "name": "n" } }
}"#;

#[test]
fn root_singleton_containers_start_empty() {
    let engine = load("rootcontainers", ROOT_CONTAINERS);
    let view = engine.view_at_head("root_view").expect("view").expect("declared");
    let row = &view.rows()[0];
    match row.field("flags") {
        Some(V::Set(members)) => assert!(members.is_empty(), "an untouched root `$set` is the EMPTY set"),
        other => panic!("§5.5/§8.2: an untouched root `$set` reads as the empty set, got {other:?}"),
    }
    match row.field("settings") {
        Some(V::Map(entries)) => assert!(entries.is_empty(), "an untouched root `map` member is the EMPTY map"),
        other => panic!("§5.5/§8.2: an untouched root `map` member reads as the empty map, got {other:?}"),
    }
    match row.field("labels") {
        Some(V::Set(members)) => assert!(members.is_empty(), "an omitted `$set` in a root struct is EMPTY"),
        other => panic!("§5.5: an omitted `$set` inside a root static struct reads as the empty set, got {other:?}"),
    }
}

#[test]
fn root_singleton_set_add_and_remove_apply_and_noop() {
    let mut engine = load("rootset", ROOT_FLAGS);
    let flags = |engine: &Engine<MemoryStore>| {
        let view = engine.view_at_head("root_view").expect("view").expect("declared");
        match view.rows()[0].field("flags") {
            Some(V::Set(members)) => members.iter().cloned().collect::<Vec<Value>>(),
            // §5.5/§8.2: a root `$set` no write has yet touched is the EMPTY set, so
            // there is exactly one admissible spelling here — `none` is absence (A.1)
            // and would mean the declared shape does not hold at a root that §8.2
            // makes durable state at every moment.
            other => panic!("a root `$set` member reads as a set, got {other:?}"),
        }
    };
    assert_eq!(flags(&engine), Vec::<Value>::new(), "nothing is held before the first write");

    assert!(matches!(
        call(&mut engine, &CallRequest::new("flag").arg("t", text("b"))),
        CallOutcome::Committed { .. }
    ));
    assert_eq!(flags(&engine), vec![text("b")], "§8.5: the member is added to the root set");

    // §8.5/§8.9: re-adding an existing member changes nothing → unchanged.
    assert!(matches!(
        call(&mut engine, &CallRequest::new("flag").arg("t", text("b"))),
        CallOutcome::Unchanged { .. }
    ));
    // Removing an absent member changes nothing → unchanged.
    assert!(matches!(
        call(&mut engine, &CallRequest::new("unflag").arg("t", text("z"))),
        CallOutcome::Unchanged { .. }
    ));

    assert!(matches!(
        call(&mut engine, &CallRequest::new("flag").arg("t", text("a"))),
        CallOutcome::Committed { .. }
    ));
    assert_eq!(flags(&engine), vec![text("a"), text("b")], "§5.5/B.1: canonical read order");

    assert!(matches!(
        call(&mut engine, &CallRequest::new("unflag").arg("t", text("a"))),
        CallOutcome::Committed { .. }
    ));
    assert_eq!(flags(&engine), vec![text("b")], "§8.5: the member is removed from the root set");
}
