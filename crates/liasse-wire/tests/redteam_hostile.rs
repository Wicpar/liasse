#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM — S5 wire-boundary robustness (attack battery item 2, wire half). The
//! engine-free wire crate is the untrusted client's whole parser, so every decode,
//! apply, and SSE parse must be TOTAL: no hostile bytes may panic, overflow the
//! stack, or over-allocate. This complements the existing `malformed`/`apply`/`sse`
//! suites with the pathological cases a red team reaches for. It is a ROBUST sign-off.

use liasse_wire::{
    ApplyError, Downstream, Occ, PatchOp, SseEvent, Upstream, WireRow, apply, decode, serde_json,
};
use serde_json::json;

fn row(id: &str, n: i64) -> WireRow {
    WireRow::new(Occ::new(id), json!(n))
}

#[test]
fn deeply_nested_json_is_an_error_not_a_stack_overflow() {
    // A naive recursive-descent parser would blow the stack here. serde's recursion
    // limit turns it into an ordinary decode error; the process survives.
    let deep = format!("{}0{}", "[".repeat(200_000), "]".repeat(200_000));
    assert!(decode::<Upstream>(&deep).is_err());
    assert!(decode::<Downstream>(&deep).is_err());
    assert!(decode::<PatchOp>(&deep).is_err());
    assert!(decode::<WireRow>(&deep).is_err());
    // Deep OBJECT nesting under a real frame field is likewise bounded.
    let nested_value = format!("{}{}", "{\"a\":".repeat(200_000), "0".to_owned() + &"}".repeat(200_000));
    let framed = format!(r#"{{"type":"init","sub":"s","rows":[{{"id":"a","value":{nested_value}}}]}}"#);
    assert!(decode::<Downstream>(&framed).is_err());
}

#[test]
fn apply_rejects_extreme_positions_without_allocating() {
    let prev = vec![row("a", 1)];

    // A position of `usize::MAX` is compared, never used to size an allocation.
    let insert = apply(&prev, &[PatchOp::Insert { at: usize::MAX, occ: Occ::new("z"), value: json!(0) }]).unwrap_err();
    assert_eq!(insert, ApplyError::PositionOutOfRange { position: usize::MAX, length: 1 });

    let mv = apply(&prev, &[PatchOp::Move { occ: Occ::new("a"), to: usize::MAX }]).unwrap_err();
    // `a` is removed first (length becomes 0), so the destination is out of range.
    assert_eq!(mv, ApplyError::PositionOutOfRange { position: usize::MAX, length: 0 });
}

#[test]
fn a_hostile_op_batch_fails_cleanly_mid_application() {
    // The batch shrinks the result, then names a position valid only for the ORIGINAL
    // length — apply reads each position in the CURRENT result, so it is caught.
    let prev = vec![row("a", 1), row("b", 2)];
    let batch = vec![
        PatchOp::Remove { occ: Occ::new("a") },       // length 2 -> 1
        PatchOp::Insert { at: 2, occ: Occ::new("c"), value: json!(3) }, // 2 > current length 1
    ];
    assert_eq!(apply(&prev, &batch), Err(ApplyError::PositionOutOfRange { position: 2, length: 1 }));

    // A duplicate insert mid-batch is caught, not silently deduped.
    let dup = vec![
        PatchOp::Insert { at: 0, occ: Occ::new("x"), value: json!(1) },
        PatchOp::Insert { at: 0, occ: Occ::new("x"), value: json!(2) },
    ];
    assert_eq!(apply(&prev, &dup), Err(ApplyError::DuplicateOccurrence { occ: Occ::new("x") }));

    // Targeting an occurrence removed earlier in the same batch is UnknownOccurrence.
    let after_remove = vec![
        PatchOp::Remove { occ: Occ::new("b") },
        PatchOp::Update { occ: Occ::new("b"), value: json!(9) },
    ];
    assert_eq!(apply(&prev, &after_remove), Err(ApplyError::UnknownOccurrence { occ: Occ::new("b") }));
}

#[test]
fn sse_parsing_is_total_on_adversarial_text() {
    let adversarial = [
        String::new(),
        "\0\0\0".to_owned(),
        "\r".repeat(10_000),                              // lone CRs
        "data".to_owned(),                                // a field with no colon
        "retry: not-a-number\n\n".to_owned(),             // non-numeric retry ignored
        "id: x".to_owned() + &"\ndata: y".repeat(100_000), // many data lines, no terminator
        ": ".to_owned() + &"c".repeat(500_000) + "\n\n",  // a huge comment
        "\u{feff}data: bom\n\n".to_owned(),               // BOM
    ];
    for text in &adversarial {
        // The only property is totality: it returns, it does not panic.
        let _events = SseEvent::parse_stream(text);
    }

    // A well-formed event survives being embedded in adversarial noise.
    let noisy = ": junk\n\0\nid: 7\ndata: {\"type\":\"frontier\"}\n\n\r\r";
    let events = SseEvent::parse_stream(noisy);
    assert!(
        events.iter().any(|e| e.id.as_deref() == Some("7") && e.data == r#"{"type":"frontier"}"#),
        "a valid event is recovered from surrounding garbage: {events:?}",
    );
}

#[test]
fn duplicate_keys_are_rejected_across_frame_families() {
    // Duplicate members must not let a hostile peer smuggle a second value past a
    // logger/proxy that saw the first — serde_json rejects them outright.
    assert!(decode::<Upstream>(r#"{"type":"hello","type":"manifest"}"#).is_err(), "duplicate tag");
    assert!(decode::<Upstream>(r#"{"type":"unsubscribe","sub":"a","sub":"b"}"#).is_err(), "duplicate field");
    assert!(decode::<Downstream>(r#"{"type":"init","sub":"s","sub":"t","rows":[]}"#).is_err());
    assert!(decode::<WireRow>(r#"{"id":"a","id":"b","value":1}"#).is_err());
    assert!(decode::<PatchOp>(r#"{"op":"remove","id":"a","id":"b"}"#).is_err());
}
