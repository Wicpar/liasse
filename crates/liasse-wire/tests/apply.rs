#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! The apply battery: [`liasse_wire::apply`] is the single §12.2 patch applier the
//! server, the client, and the corpus share. Each expected result here is computed
//! by hand from the §12.2 rules (each `$at`/`$to` read in the current result), never
//! from the function's own output, so the assertions are externally deducible.

use liasse_wire::{ApplyError, Occ, PatchOp, Value, WireRow, apply, serde_json};
use serde_json::json;

fn row(id: &str, n: i64) -> WireRow {
    WireRow::new(Occ::new(id), json!(n))
}

/// The occurrence tokens of a result, in order — what §12.2 fixes.
fn ids(rows: &[WireRow]) -> Vec<String> {
    rows.iter().map(|r| r.occ().as_str().to_owned()).collect()
}

fn value_of<'a>(rows: &'a [WireRow], id: &str) -> Option<&'a Value> {
    rows.iter().find(|r| r.occ().as_str() == id).map(WireRow::value)
}

#[test]
fn empty_patch_is_the_frontier_only_no_op() {
    let prev = vec![row("a", 1), row("b", 2)];
    let next = apply(&prev, &[]).expect("empty patch applies");
    assert_eq!(next, prev, "a frontier-only patch leaves the result identical");
}

#[test]
fn insert_places_the_row_at_its_position() {
    let prev = vec![row("a", 1), row("b", 2)];

    let at_start = apply(&prev, &[PatchOp::Insert { at: 0, occ: Occ::new("c"), value: json!(9) }]).unwrap();
    assert_eq!(ids(&at_start), ["c", "a", "b"]);

    let at_mid = apply(&prev, &[PatchOp::Insert { at: 1, occ: Occ::new("c"), value: json!(9) }]).unwrap();
    assert_eq!(ids(&at_mid), ["a", "c", "b"]);

    let at_end = apply(&prev, &[PatchOp::Insert { at: 2, occ: Occ::new("c"), value: json!(9) }]).unwrap();
    assert_eq!(ids(&at_end), ["a", "b", "c"]);
    assert_eq!(value_of(&at_end, "c"), Some(&json!(9)));
}

#[test]
fn remove_drops_the_occurrence() {
    let prev = vec![row("a", 1), row("b", 2), row("c", 3)];
    let next = apply(&prev, &[PatchOp::Remove { occ: Occ::new("b") }]).unwrap();
    assert_eq!(ids(&next), ["a", "c"]);
}

#[test]
fn move_reorders_in_both_directions() {
    let prev = vec![row("a", 1), row("b", 2), row("c", 3)];

    // Forward: remove `a` (-> [b,c]) then insert at 2 -> [b,c,a].
    let forward = apply(&prev, &[PatchOp::Move { occ: Occ::new("a"), to: 2 }]).unwrap();
    assert_eq!(ids(&forward), ["b", "c", "a"]);

    // Backward: remove `c` (-> [a,b]) then insert at 0 -> [c,a,b].
    let backward = apply(&prev, &[PatchOp::Move { occ: Occ::new("c"), to: 0 }]).unwrap();
    assert_eq!(ids(&backward), ["c", "a", "b"]);
}

#[test]
fn update_replaces_value_and_preserves_position() {
    let prev = vec![row("a", 1), row("b", 2)];
    let next = apply(&prev, &[PatchOp::Update { occ: Occ::new("a"), value: json!(99) }]).unwrap();
    assert_eq!(ids(&next), ["a", "b"], "update never moves a row");
    assert_eq!(value_of(&next, "a"), Some(&json!(99)));
    assert_eq!(value_of(&next, "b"), Some(&json!(2)));
}

#[test]
fn combined_update_and_move_lands_the_changed_value_at_the_moved_position() {
    // Mirrors a diff of a row whose projected value AND sort key changed: an
    // in-place update of `b`, then a move of `c` to the front.
    let prev = vec![row("a", 1), row("b", 2), row("c", 3)];
    let ops = vec![
        PatchOp::Update { occ: Occ::new("b"), value: json!(20) },
        PatchOp::Move { occ: Occ::new("c"), to: 0 },
    ];
    let next = apply(&prev, &ops).unwrap();
    assert_eq!(ids(&next), ["c", "a", "b"]);
    assert_eq!(value_of(&next, "b"), Some(&json!(20)), "the updated value moved with the row");
}

#[test]
fn rekey_preserves_the_occurrence_and_order() {
    let prev = vec![row("a", 1), row("b", 2)];
    let next = apply(&prev, &[PatchOp::Rekey { occ: Occ::new("a"), key: json!("renamed") }]).unwrap();
    assert_eq!(ids(&next), ["a", "b"], "a rekey keeps the occurrence and its position");
}

#[test]
fn insert_past_the_end_is_out_of_range() {
    let prev = vec![row("a", 1)];
    let err = apply(&prev, &[PatchOp::Insert { at: 5, occ: Occ::new("c"), value: json!(0) }]).unwrap_err();
    assert_eq!(err, ApplyError::PositionOutOfRange { position: 5, length: 1 });
}

#[test]
fn move_past_the_end_of_the_shrunken_result_is_out_of_range() {
    // After removing `a`, the working length is 1, so `to: 5` is out of range.
    let prev = vec![row("a", 1), row("b", 2)];
    let err = apply(&prev, &[PatchOp::Move { occ: Occ::new("a"), to: 5 }]).unwrap_err();
    assert_eq!(err, ApplyError::PositionOutOfRange { position: 5, length: 1 });
}

#[test]
fn targeting_an_absent_occurrence_is_rejected() {
    let prev = vec![row("a", 1)];
    for op in [
        PatchOp::Remove { occ: Occ::new("x") },
        PatchOp::Move { occ: Occ::new("x"), to: 0 },
        PatchOp::Update { occ: Occ::new("x"), value: json!(0) },
        PatchOp::Rekey { occ: Occ::new("x"), key: json!("k") },
    ] {
        let err = apply(&prev, std::slice::from_ref(&op)).unwrap_err();
        assert_eq!(err, ApplyError::UnknownOccurrence { occ: Occ::new("x") }, "for {op:?}");
    }
}

#[test]
fn inserting_a_present_occurrence_is_rejected() {
    let prev = vec![row("a", 1)];
    let err = apply(&prev, &[PatchOp::Insert { at: 0, occ: Occ::new("a"), value: json!(2) }]).unwrap_err();
    assert_eq!(err, ApplyError::DuplicateOccurrence { occ: Occ::new("a") });
}
