#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM (§5.9/§B.5/§5.4/§22.1/§20.1): an enum used as a collection KEY
//! component, whose labels a migration REORDERS, strands every live row at its
//! OLD-ordinal address while re-deriving its key FIELD to the new ordinal — so
//! the committed state is neither in canonical key order nor addressable by its
//! own key.
//!
//! This is the REORDER sibling of the enum-narrowing family (`redteam_enum_
//! narrowing_migration`, `redteam_struct_nested_enum_narrowing_migration`) that
//! the Wave-14/Wave-15 fixes closed. Wave-15 (e55110d) REWROTE `rules::coerce_value`
//! to re-derive every migrated enum leaf's declaration-order ordinal against the
//! target's current set ("a retained label re-derives its declaration-order
//! ordinal, so a reorder settles on the current position"). When that enum leaf is
//! a KEY component, re-deriving its ordinal changes the row's canonical key — but
//! the row was already addressed in `build_migrated` from the OLD ordinal:
//!
//!   migrate.rs `build_migrated`: `map_row` copies the enum verbatim (old ordinal)
//!   -> `key_address(..)` fixes the RowAddress from that old-ordinal key
//!   -> LATER `coerce_and_require` re-derives the field to the NEW ordinal and
//!      `prospective.replace(address, fields)` keeps the STALE address.
//!
//! Result: the row's stored key (address) and its key field value diverge.
//!
//! §5.9: "Enum values are checked labels, and their default total order follows
//! that [declaration] order." §B.5: "A collection defaults to key ascending."
//! §22.1 state constraints: "collection keys ... hold in every committed state."
//! §5.4/§8.5: a keyed delete addresses a row by its application-visible key. Every
//! expectation below is deducible from that spec text alone.
//!
//! The migration is accepted as a `Patch` (a pure label reorder), so the spec-
//! correct committed state must present rows in the TARGET's canonical key order
//! and keep each row addressable by its current key. The engine does neither.

mod support;

use liasse_runtime::{CallRequest, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, store};

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// Composite key `[status, id]` with `status` an enum. Declaration order
/// draft(0), active(1), archived(2). Two live rows: (draft, t1) and (archived, t2).
const V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.enumkey.reorder@1.0.0",
  "$model": {
    "things": {
      "$key": ["status", "id"],
      "status": { "$enum": ["draft", "active", "archived"] },
      "id": "text"
    },
    "all": { "$view": ".things { id, status }" },
    "$mut": { "del": ".things - { status: @status, id: @id }" }
  },
  "$data": { "things": { "draft:t1": {}, "archived:t2": {} } }
}"#;

/// A pure label REORDER (retains every label, changes declaration order):
/// archived(0), draft(1), active(2). Classified `Patch` (compatible).
const V2_REORDER: &str = r#"{
  "$liasse": 1,
  "$app": "t.enumkey.reorder@1.0.1",
  "$model": {
    "things": {
      "$key": ["status", "id"],
      "status": { "$enum": ["archived", "draft", "active"] },
      "id": "text"
    },
    "all": { "$view": ".things { id, status }" },
    "$mut": { "del": ".things - { status: @status, id: @id }" }
  }
}"#;

fn load(instance: &str, def: &str) -> liasse_runtime::Engine<MemoryStore> {
    let mut g = generator();
    liasse_runtime::Engine::load(store(instance), def, &mut g).expect("load")
}

/// The `(status-label, id)` pairs in the view's default (key-ascending) order.
fn order(engine: &liasse_runtime::Engine<MemoryStore>) -> Vec<(String, String)> {
    let view = engine.view_at_head("all").expect("view ok").expect("declared");
    view.rows()
        .iter()
        .map(|row| {
            let status = match row.field("status") {
                Some(Value::Enum(e)) => e.label().to_owned(),
                other => panic!("status is an enum, got {other:?}"),
            };
            let id = match row.field("id") {
                Some(Value::Text(t)) => t.as_str().to_owned(),
                other => panic!("id is text, got {other:?}"),
            };
            (status, id)
        })
        .collect()
}

#[test]
fn enum_key_reorder_migration_keeps_rows_canonical_and_addressable() {
    let mut engine = load("enumkey-reorder", V1);

    // §B.5/§5.9: under V1 (draft<active<archived) the canonical key-ascending order
    // lists draft(0,t1) before archived(2,t2).
    assert_eq!(
        order(&engine),
        vec![("draft".to_owned(), "t1".to_owned()), ("archived".to_owned(), "t2".to_owned())],
        "V1 rows must already be in canonical key order",
    );

    let mut g = generator();
    let report = engine.update(V2_REORDER, &mut g).expect("the label reorder is accepted (Patch)");
    assert_eq!(format!("{:?}", report.relation), "Patch", "a pure label reorder is a compatible Patch");

    // §5.9/§B.5/§22.1: after the migration commits, the target enum order is
    // archived(0) < draft(1) < active(2), so the canonical key-ascending order MUST
    // now list archived(0,t2) BEFORE draft(1,t1). The rows were re-derived to the new
    // ordinals (the view renders archived=0, draft=1) but remain stored at their OLD
    // addresses, so the engine returns them in the stale physical order instead.
    let observed = order(&engine);
    assert_eq!(
        observed,
        vec![("archived".to_owned(), "t2".to_owned()), ("draft".to_owned(), "t1".to_owned())],
        "BUG (§5.9/§B.5/§22.1): after an enum-KEY reorder the committed rows are NOT in canonical \
         key order. `coerce_and_require` re-derived each row's `status` ordinal (view shows the new \
         ordinals) but `build_migrated` had already fixed the RowAddress from the OLD ordinal and never \
         re-keyed, so the rows enumerate in the stale order {observed:?} instead of \
         [(archived,t2),(draft,t1)].",
    );

    // §5.4/§8.5/§22.1: each row must be addressable by its CURRENT canonical key.
    // Deleting (status=archived, id=t2) must remove that row. With the stale address
    // (archived stored at old ordinal 2, looked up at new ordinal 0) the keyed delete
    // MISSES and silently no-ops — the row is unaddressable and can never be deleted
    // or mutated by key.
    let outcome = {
        let mut g2 = generator();
        engine
            .call(&CallRequest::new("del").arg("status", text("archived")).arg("id", text("t2")), &mut g2)
            .expect("call")
    };
    let survived = order(&engine).iter().any(|(status, id)| status == "archived" && id == "t2");
    assert!(
        !survived,
        "BUG (§5.4/§8.5/§22.1): the row (archived,t2) is NOT addressable by its own current key after \
         the reorder migration — a delete by that exact key no-opped ({outcome:?}) and the row survives, \
         so a committed row can never be removed or mutated by its key.",
    );
}
