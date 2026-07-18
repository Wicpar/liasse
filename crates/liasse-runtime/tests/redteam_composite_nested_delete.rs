#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probe: the direct `collection - keys` delete form (§8.5) fails to
//! remove a **nested composite-keyed** row, because the nested-collection branch
//! of `exec_delete` re-wraps the already-positional composite key with
//! `KeyValue::single(..)` — the exact addressing defect commit 3fdb601 fixed for
//! `exec_erase`, left unfixed on this sibling delete path.
//!
//! Spec chain (all normative):
//!   * §8.5 (SPEC.md line 1022): `collection - keys   delete rows by key` — a
//!     delete addressing an existing key removes that row.
//!   * §5.4 (line 455): a nested collection's row has "row identity plus ancestor
//!     identity" — a nested collection is a first-class keyed collection whose
//!     `$key` MAY be composite (line 442, the `vat_rates` composite example).
//!   * §6.3 (line 698) / A.9 (line 4479): a composite-key lookup names each key
//!     component; the composite key is the positional tuple in `$key` order.
//!
//! Root cause — `liasse-runtime/src/interp.rs`, `exec_delete`:
//!
//! ```ignore
//! if loc.decl.len() > 1 {
//!     for key in targets {
//!         self.remove_subtree(&loc.store_path.row(KeyValue::single(key)));
//!     }
//!     return Ok(());
//! }
//! ```
//!
//! For a nested collection (`loc.decl.len() > 1`) `key` is the application-visible
//! identity, which for a composite key is the positional
//! `Value::Composite([floor, code])` that `normalize_key_operand` produced.
//! `KeyValue::single(Value::Composite([floor, code]))` builds a ONE-component
//! `KeyValue { first: Composite([floor, code]), rest: [] }`. The row was stored
//! (via `materialize::row_key`) under the N-component
//! `KeyValue::composite(floor, [code]) = { first: floor, rest: [code] }`. The two
//! addresses differ, so `remove_subtree`'s `self.prospective.contains(address)`
//! guard misses, returns early, and removes nothing — the delete silently no-ops
//! and the nested composite row survives. The scalar analogue works (a
//! single-field key stores as `{ first: key, rest: [] }`, which `KeyValue::single`
//! reproduces exactly), and the identical fix already exists as
//! `materialize::key_value_of`, so this is a composite-carrier defect on the
//! nested delete path, not an unsupported form. Expectations are re-derived from
//! §8.5 / §5.4 / §6.3, not the implementation.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// A company → rooms/docs two-level model. `rooms` is nested and **composite**
/// keyed `["floor", "code"]`; `docs` is nested and **single** keyed `slug`. Each
/// nested collection is flattened by a `::` root view so the surviving/removed
/// rows are observable at head. The direct nested delete forms address a row by
/// its authoring key operand.
const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.compnesteddel@1.0.0",
  "$model": {
    "companies": {
      "$key": "id",
      "id": "text",
      "rooms": {
        "$key": ["floor", "code"],
        "floor": "text",
        "code": "text",
        "name": "text"
      },
      "docs": {
        "$key": "slug",
        "slug": "text",
        "title": "text"
      }
    },
    "all_rooms": { "$view": ".companies::rooms { floor, code, name, $sort: [floor, code] }" },
    "all_docs": { "$view": ".companies::docs { slug, title, $sort: [slug] }" },
    "$mut": {
      "add_company": ".companies + { id: @id }",
      "add_room": ".companies[@company].rooms + { floor: @floor, code: @code, name: @name }",
      "add_doc": ".companies[@company].docs + { slug: @slug, title: @title }",
      "del_room": ".companies[@company].rooms - { floor: @floor, code: @code }",
      "del_doc": ".companies[@company].docs - @slug"
    }
  }
}"#;

fn commit(outcome: CallOutcome) {
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "expected a commit, got {outcome:?}");
}

fn room_count(engine: &Engine<MemoryStore>) -> usize {
    engine.view_at_head("all_rooms").expect("view").expect("declared").rows().len()
}

fn doc_count(engine: &Engine<MemoryStore>) -> usize {
    engine.view_at_head("all_docs").expect("view").expect("declared").rows().len()
}

/// Load the model with one company `acme` holding one composite-keyed room
/// `[1, a]` named "Main".
fn with_room(label: &str) -> Engine<MemoryStore> {
    let mut g = generator();
    let mut engine = load(label, M);
    commit(engine.call(&CallRequest::new("add_company").arg("id", text("acme")), &mut g).expect("call"));
    commit(
        engine
            .call(
                &CallRequest::new("add_room")
                    .arg("company", text("acme"))
                    .arg("floor", text("1"))
                    .arg("code", text("a"))
                    .arg("name", text("Main")),
                &mut g,
            )
            .expect("call"),
    );
    assert_eq!(room_count(&engine), 1, "fixture seeds exactly one nested composite room [1, a]");
    engine
}

#[test]
fn direct_delete_removes_nested_composite_row() {
    // §8.5/§5.4: `.companies[acme].rooms - { floor: '1', code: 'a' }` deletes the
    // nested room whose composite key is [1, a]. That row exists, so `all_rooms`
    // MUST be empty afterwards. (Currently the nested branch of `exec_delete`
    // addresses it with `KeyValue::single(Value::Composite([1, a]))`, which never
    // equals the stored N-component key, so the row survives.)
    let mut engine = with_room("nested-comp-del");
    let mut g = generator();
    let outcome = engine
        .call(
            &CallRequest::new("del_room")
                .arg("company", text("acme"))
                .arg("floor", text("1"))
                .arg("code", text("a")),
            &mut g,
        )
        .expect("call");
    assert_eq!(
        room_count(&engine),
        0,
        "§8.5/§5.4: the direct `rooms - {{ floor, code }}` form must delete the nested \
         composite row [1, a]; it survived (delete outcome: {outcome:?})"
    );
}

#[test]
fn control_direct_delete_removes_nested_single_key_row() {
    // CONTROL (passes): the SAME nested-collection direct-delete path removes a
    // SINGLE-field-keyed nested row (`docs - @slug`). A single-field key stores as
    // `{ first: slug, rest: [] }`, which `KeyValue::single(slug)` reproduces
    // exactly — so the nested delete path itself works. This isolates the defect
    // above to the composite-key carrier, not the nested form as such.
    let mut engine = load("nested-single-del", M);
    let mut g = generator();
    commit(engine.call(&CallRequest::new("add_company").arg("id", text("acme")), &mut g).expect("call"));
    commit(
        engine
            .call(
                &CallRequest::new("add_doc")
                    .arg("company", text("acme"))
                    .arg("slug", text("readme"))
                    .arg("title", text("Read me")),
                &mut g,
            )
            .expect("call"),
    );
    assert_eq!(doc_count(&engine), 1, "one nested single-keyed doc seeded");
    commit(
        engine
            .call(&CallRequest::new("del_doc").arg("company", text("acme")).arg("slug", text("readme")), &mut g)
            .expect("call"),
    );
    assert_eq!(doc_count(&engine), 0, "the nested single-keyed doc is deleted by the direct form");
}
