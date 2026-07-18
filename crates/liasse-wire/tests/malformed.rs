#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Hostile-input battery: decoding is the trust boundary (AGENTS.md — parse every
//! inbound frame as hostile input). Every case here MUST return `Err`, never panic,
//! and never abort the process. The strings are adversarial: truncated JSON,
//! unknown tags, wrong types, missing fields, wrong-sign integers, deep nesting,
//! and raw garbage.

use liasse_wire::{Downstream, Occ, Outcome, PatchOp, Upstream, WireRow, decode};

/// A `decode` that must fail for the requested type, reported with the input.
fn must_reject<T: serde::de::DeserializeOwned>(text: &str) {
    assert!(decode::<T>(text).is_err(), "must reject: {text:?}");
}

#[test]
fn upstream_frames_reject_malformed_input() {
    must_reject::<Upstream>("");
    must_reject::<Upstream>("not json at all");
    must_reject::<Upstream>("{"); // truncated
    must_reject::<Upstream>("{}"); // no type tag
    must_reject::<Upstream>(r#"{"type":"nonesuch"}"#); // unknown tag
    must_reject::<Upstream>(r#"{"type":"view"}"#); // missing required sub/address
    must_reject::<Upstream>(r#"{"type":"call","address":"a"}"#); // missing args
    must_reject::<Upstream>(r#"{"type":"unsubscribe","sub":42}"#); // sub is not a string
    must_reject::<Upstream>(r#"["type","view"]"#); // an array, not an object
    must_reject::<Upstream>(r#"{"type":"operation","operation":null}"#); // null token
}

#[test]
fn downstream_frames_reject_malformed_input() {
    must_reject::<Downstream>(r#"{"type":"init","sub":"s"}"#); // missing rows
    must_reject::<Downstream>(r#"{"type":"init","sub":"s","rows":{}}"#); // rows not an array
    must_reject::<Downstream>(r#"{"type":"close","sub":"s","reason":"made-up"}"#); // unknown reason
    must_reject::<Downstream>(r#"{"type":"fault","code":"nope","message":"m"}"#); // unknown code
    must_reject::<Downstream>("null");
    must_reject::<Downstream>("123");
}

#[test]
fn patch_ops_reject_malformed_input() {
    must_reject::<PatchOp>(r#"{"op":"insert","at":0,"id":"a"}"#); // missing value
    must_reject::<PatchOp>(r#"{"op":"insert","at":-1,"id":"a","value":1}"#); // negative position
    must_reject::<PatchOp>(r#"{"op":"insert","at":1.5,"id":"a","value":1}"#); // non-integer position
    must_reject::<PatchOp>(r#"{"op":"move","id":"a"}"#); // missing to
    must_reject::<PatchOp>(r#"{"op":"unknown","id":"a"}"#); // unknown op tag
    must_reject::<PatchOp>(r#"{"id":"a","value":1}"#); // no op tag
}

#[test]
fn strict_structs_reject_unknown_members() {
    // WireRow is a plain struct (not an enum variant), so it denies unknown fields.
    must_reject::<WireRow>(r#"{"id":"a","value":1,"sneaky":true}"#);
    must_reject::<WireRow>(r#"{"id":"a"}"#); // missing value
    must_reject::<WireRow>(r#"{"value":1}"#); // missing id
}

#[test]
fn outcome_frames_reject_malformed_input() {
    must_reject::<Outcome>(r#"{"status":"committed"}"#); // missing frontier/commit
    must_reject::<Outcome>(r#"{"status":"failed","code":"whoops"}"#); // unknown failed code
    must_reject::<Outcome>(r#"{"status":"invented"}"#); // unknown status
}

#[test]
fn a_broad_adversarial_sweep_never_panics() {
    // The point is total decoding: whatever these produce, decoding returns a
    // Result and the process survives. Reaching the final assert is the proof.
    let nasty = [
        "",
        " ",
        "\0",
        "\u{feff}",
        "[[[[[[[[[[",
        "{\"type\":",
        "{\"type\":\"view\",\"sub\":\"s\",\"address\":\"a\",\"window\":{\"size\":-4}}",
        &"a".repeat(10_000),
        &format!("{{\"type\":\"init\",\"sub\":\"s\",\"rows\":[{}]}}", "1,".repeat(1_000)),
        "\"just a string\"",
        "true",
        "1e999999",
    ];
    for input in nasty {
        let _up = decode::<Upstream>(input);
        let _down = decode::<Downstream>(input);
        let _out = decode::<Outcome>(input);
        let _op = decode::<PatchOp>(input);
        let _tok = decode::<Occ>(input);
    }
    // Reaching here without a panic is the property under test: decoding is total.
}
