#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13.13 set rule: a bundled `$set` reconciles by MEMBERSHIP, not by whole value.
//!
//! "Sets add members newly present in the new bundle and remove old bundled members
//! only when application state still reflects the old bundle membership."
//!
//! That is a per-member three-way merge, which is strictly weaker than the
//! per-field rule the sentence before it states for a "bundled scalar or struct
//! field". Comparing a set as one opaque value collapses it back into the scalar
//! rule: a single locally added tag makes the whole current set differ from the old
//! bundled set, so the field reads as "locally modified", the current value is
//! retained WHOLE, and every member the new bundle adds is silently lost — the
//! release can never extend a set once any instance has touched it.
//!
//! The membership rule instead resolves each member on its own:
//!
//! * in NEW but not in OLD: ADDED — a member the release newly bundles.
//! * in OLD but not in NEW: REMOVED, but only where the member is still present
//!   (application state still reflects the old bundle membership); a member already
//!   removed locally stays removed and is never re-added.
//! * in NEITHER: a locally added member, RETAINED.
//! * in BOTH: untouched either way, so a local removal of a member the release still
//!   bundles stays removed.
//!
//! §13.13 is scoped by ADDRESS, so the rule holds for a `$set` in a keyed-collection
//! row and for a `$set` at a §8.2 root-singleton member alike. Annex B: the merged
//! value is a SET — its read order is the element type's canonical order, never the
//! order the bundle listed members in.

mod support;

use liasse_runtime::{CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

/// One bundled keyed collection whose rows carry a `$set`, plus a bundled `$set` at
/// a §8.2 root-singleton address — the two containers §13.13 scopes the rule over.
const MODEL: &str = r#"
    "$model": {
      "docs": {
        "$key": "id",
        "id": "text",
        "tags": { "$set": "text" },
        "$mut": { "tag": ".tags + @t", "untag": ".tags - @t" }
      },
      "flags": { "$set": "text" },
      "$mut": { "flag": ".flags + @t", "unflag": ".flags - @t" },
      "docs_view": { "$view": ".docs { id, tags }" },
      "root_view": { "$view": ". { flags }" }
    }"#;

fn definition(version: &str, bundle: &str) -> String {
    format!(r#"{{ "$liasse": 1, "$app": "t.bundleset@{version}",{MODEL}, "$bundle": {bundle} }}"#)
}

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// The members of the `docs/d1` row's `tags` set, in canonical read order.
fn tags(engine: &Engine<MemoryStore>) -> Vec<String> {
    let view = engine.view_at_head("docs_view").expect("view evaluates").expect("declared");
    members(view.rows().first().and_then(|row| row.field("tags")))
}

/// The members of the root-singleton `flags` set, in canonical read order.
fn flags(engine: &Engine<MemoryStore>) -> Vec<String> {
    let view = engine.view_at_head("root_view").expect("view evaluates").expect("declared");
    members(view.rows().first().and_then(|row| row.field("flags")))
}

fn members(field: Option<&Value>) -> Vec<String> {
    match field {
        Some(Value::Set(set)) => set.iter().map(|value| value.to_canonical_json_string()).collect(),
        other => panic!("expected a set value, got {other:?}"),
    }
}

fn call(engine: &mut Engine<MemoryStore>, request: &CallRequest) {
    let mut generator = generator();
    engine.call(request, &mut generator).expect("the mutation commits");
}

/// REPRODUCTION (collection row): the full membership matrix in one update.
///
/// old bundle `{x, y}`; the instance adds `local` and removes `y`; new bundle
/// `{x, z}`. Every arm of the rule fires at once:
///   x     — bundled by both, still held        -> unchanged;
///   y     — dropped by the release AND already removed locally -> stays removed;
///   z     — newly bundled                      -> ADDED;
///   local — in neither bundle                  -> RETAINED.
///
/// Before the fix the set was compared whole: `{local, x} != {x, y}` read as a
/// local edit, so the entire current value was retained and `z` never arrived.
#[test]
fn bundled_collection_set_merges_by_membership() {
    let mut engine = load(
        "bundle-set-row",
        &definition("1.0.0", r#"{ "docs": { "d1": { "tags": ["x", "y"] } } }"#),
    );
    assert_eq!(tags(&engine), vec!["\"x\"", "\"y\""], "genesis applies the bundled set");

    call(&mut engine, &CallRequest::new("tag").receiver(text("d1")).arg("t", text("local")));
    call(&mut engine, &CallRequest::new("untag").receiver(text("d1")).arg("t", text("y")));
    assert_eq!(tags(&engine), vec!["\"local\"", "\"x\""], "the local edits landed");

    let mut generator = generator();
    engine
        .update(&definition("1.1.0", r#"{ "docs": { "d1": { "tags": ["x", "z"] } } }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        tags(&engine),
        vec!["\"local\"", "\"x\"", "\"z\""],
        "§13.13: `z` is newly bundled so it is ADDED; `local` is not bundled at all so it is \
         RETAINED; `y` was already removed locally so the release's removal finds nothing to do \
         and never re-adds it; Annex B: the result reads in the element type's canonical order",
    );
}

/// REPRODUCTION (root singleton): the same matrix one container up — §13.13 scopes
/// the rule by address, and §8.2 makes a root `$set` durable state at its own
/// address.
#[test]
fn bundled_root_singleton_set_merges_by_membership() {
    let mut engine = load("bundle-set-root", &definition("1.0.0", r#"{ "flags": ["x", "y"] }"#));
    assert_eq!(flags(&engine), vec!["\"x\"", "\"y\""], "genesis applies the bundled root set");

    call(&mut engine, &CallRequest::new("flag").arg("t", text("local")));
    call(&mut engine, &CallRequest::new("unflag").arg("t", text("y")));
    assert_eq!(flags(&engine), vec!["\"local\"", "\"x\""], "the local root-set edits landed");

    let mut generator = generator();
    engine
        .update(&definition("1.1.0", r#"{ "flags": ["x", "z"] }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        flags(&engine),
        vec!["\"local\"", "\"x\"", "\"z\""],
        "§13.13/§8.2: the membership rule applies to a bundled root-singleton set exactly as to a \
         bundled collection-row set",
    );
}

/// §13.13 removal arm in isolation: a member the new bundle DROPPED is removed
/// where "application state still reflects the old bundle membership" — the
/// instance never touched it, so it goes.
#[test]
fn bundled_set_removes_a_dropped_member_the_instance_still_holds() {
    let mut engine = load(
        "bundle-set-drop",
        &definition("1.0.0", r#"{ "docs": { "d1": { "tags": ["x", "y"] } } }"#),
    );
    let mut generator = generator();

    engine
        .update(&definition("1.1.0", r#"{ "docs": { "d1": { "tags": ["x"] } } }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        tags(&engine),
        vec!["\"x\""],
        "§13.13: the release dropped `y` and the instance still reflected the old bundle \
         membership, so `y` is removed",
    );
}

/// The other side of the removal arm: a locally ADDED member is never removed by a
/// release that does not bundle it — application state does not reflect any old
/// bundle membership for it.
#[test]
fn bundled_set_retains_a_locally_added_member_across_a_drop() {
    let mut engine = load(
        "bundle-set-keep-local",
        &definition("1.0.0", r#"{ "docs": { "d1": { "tags": ["x", "y"] } } }"#),
    );
    call(&mut engine, &CallRequest::new("tag").receiver(text("d1")).arg("t", text("mine")));
    let mut generator = generator();

    engine
        .update(&definition("1.1.0", r#"{ "docs": { "d1": { "tags": ["x"] } } }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        tags(&engine),
        vec!["\"mine\"", "\"x\""],
        "§13.13: `y` was old-bundled and still held, so it is removed; `mine` was never bundled, \
         so it is retained as local data",
    );
}

/// A member the instance removed locally while the release STILL bundles it stays
/// removed: the new bundle does not re-add a member that is neither newly present
/// nor a resolvable removal — "an existing row, field, or member is never modified"
/// is the `$seed` rule, and `$bundle`'s own rule only ADDS what is *newly* present.
#[test]
fn bundled_set_does_not_resurrect_a_locally_removed_member() {
    let mut engine = load(
        "bundle-set-no-resurrect",
        &definition("1.0.0", r#"{ "docs": { "d1": { "tags": ["x", "y"] } } }"#),
    );
    call(&mut engine, &CallRequest::new("untag").receiver(text("d1")).arg("t", text("y")));
    let mut generator = generator();

    engine
        .update(&definition("1.1.0", r#"{ "docs": { "d1": { "tags": ["x", "y"] } } }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        tags(&engine),
        vec!["\"x\""],
        "§13.13: `y` is present in BOTH bundles, so it is neither newly added nor a removal — the \
         local removal is retained",
    );
}

/// A set a release bundles for the FIRST time at an address that has never held one
/// fills, exactly as a newly bundled scalar does ("holding nothing compares equal to
/// holding nothing"). An omitted set is the EMPTY set (§5.5), so this is the
/// old-empty / new-nonempty arm of the membership rule.
#[test]
fn newly_bundled_set_members_fill_an_untouched_set() {
    let mut engine = load("bundle-set-new", &definition("1.0.0", r#"{ "docs": { "d1": {} } }"#));
    assert_eq!(tags(&engine), Vec::<String>::new(), "§5.5: an omitted set starts empty");
    let mut generator = generator();

    engine
        .update(&definition("1.1.0", r#"{ "docs": { "d1": { "tags": ["a", "b"] } } }"#), &mut generator)
        .expect("the compatible minor update commits");

    assert_eq!(
        tags(&engine),
        vec!["\"a\"", "\"b\""],
        "§13.13: members newly present in the new bundle are added to an untouched set",
    );
}
