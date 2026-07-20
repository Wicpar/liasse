#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM (WAVE 4) — TARGET 2, root-cause localization.
//!
//! The corpus probe `redteam_w4_nested_view_member_drop` shows the projection
//! member `kids: .children { … }` is dropped from a view result. This test pins
//! the drop to the RUNTIME view-output path, independent of the testkit adapter:
//! it reads `Engine::view_at_head` directly and inspects the projected row.
//!
//! §7.1 (SPEC.md:804/817): a projection member may be a nested structure and
//! "Projection members are unordered named outputs" — a declared member is an
//! output of the row. `.parents { id, kids: .children { id, label } }` therefore
//! projects `kids` onto every parent row.
//!
//! Root cause: `cell_field_value` (crates/liasse-runtime/src/view.rs:257) maps a
//! keyed nested cell — `Cell::Collection(_)` (a sub-collection view) or a keyed
//! `Cell::Row(_)` — to `None`, dropping it from the `ViewRow::fields` the view
//! delivers. There is no other carrier for nested cells on `ViewRow`, so the
//! nested collection is simply absent from the materialized view value. The
//! 320b767 row-materialization unification touched check/on-delete/cascade
//! materialization, NOT this view-output path, so it did not fix this.

mod support;

use liasse_runtime::Engine;
use liasse_store::MemoryStore;
use support::load;

// A parent collection with a nested child collection, projected as a named
// sub-collection member. Seeded with one parent holding one child.
const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.rt.nestview.direct@1.0.0",
  "$model": {
    "parents": {
      "$key": "id",
      "id": "text",
      "children": { "$key": "id", "id": "text", "label": "text" }
    },
    "tree": { "$view": ".parents { id, kids: .children { id, label } }" }
  },
  "$data": { "parents": { "p1": { "children": { "c1": { "label": "x" } } } } }
}"#;

/// §7.1: the projected parent row MUST carry the nested sub-collection member
/// `kids`. The runtime view path drops keyed nested cells (view.rs:263), so
/// `kids` is absent from the delivered `ViewRow` — this assertion FAILS, pinning
/// the §7/§12 member-drop to the runtime view-output path (not the adapter, not
/// meters). The scalar `id` member is present, isolating "keyed nested cell" as
/// the sole dropped kind.
#[test]
fn runtime_view_projects_nested_subcollection_member() {
    let engine: Engine<MemoryStore> = load("nestview-direct", M);
    let view = engine.view_at_head("tree").expect("view evaluates").expect("view declared");
    let rows = view.rows();
    assert_eq!(rows.len(), 1, "one parent row");
    let p1 = &rows[0];
    assert!(p1.field("id").is_some(), "the scalar `id` member is projected");
    assert!(
        p1.field("kids").is_some(),
        "§7.1: the nested sub-collection member `kids` MUST be a projected output; \
         view.rs::cell_field_value drops keyed nested cells (Cell::Collection / keyed Cell::Row)",
    );
}
