#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! The wire schema is a contract: every consumer (server codec, WASM client,
//! corpus) reads the same field names and tags. These tests pin that contract to
//! the externally specified shapes (the adopted plan's `{op:insert,at,id,value}`,
//! `WireRow = {id, value}`, tagged frames) and then prove the codec round-trips
//! every frame and every patch operation. The expected JSON is the plan's, not the
//! program's own output, so the assertion is externally deducible.

use liasse_wire::{
    Code, CloseReason, ConnectionToken, Downstream, FailedCode, FaultCode, Ft, Occ, Outcome,
    PatchOp, ResetReason, Sub, Upstream, Value, WireAnchor, WireRow, WireWindow, decode, encode,
    serde_json,
};
use serde_json::json;

fn json_of<T: serde::Serialize>(value: &T) -> Value {
    serde_json::from_str(&encode(value).expect("encode")).expect("re-parse as json")
}

fn round_trip<T>(value: T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let text = encode(&value).expect("encode");
    let back: T = decode(&text).expect("decode");
    assert_eq!(value, back, "value must survive an encode/decode round trip");
}

#[test]
fn wire_row_uses_id_and_value_members_only() {
    let row = WireRow::new(Occ::new("o1"), json!({ "title": "hi", "n": 3 }));
    let shape = json_of(&row);
    assert_eq!(shape.get("id").and_then(Value::as_str), Some("o1"), "occ travels as `id`");
    assert_eq!(shape.get("value"), Some(&json!({ "title": "hi", "n": 3 })));
    assert!(shape.get("occ").is_none(), "the Rust field name must not leak to the wire");
    assert_eq!(shape.as_object().map(|o| o.len()), Some(2), "only id and value on the wire");
    round_trip(row);
}

#[test]
fn patch_ops_are_op_tagged_with_id_member() {
    let insert = PatchOp::Insert { at: 2, occ: Occ::new("o1"), value: json!("v") };
    let shape = json_of(&insert);
    assert_eq!(shape["op"], json!("insert"));
    assert_eq!(shape["at"], json!(2));
    assert_eq!(shape["id"], json!("o1"));
    assert_eq!(shape["value"], json!("v"));

    assert_eq!(json_of(&PatchOp::Remove { occ: Occ::new("o1") })["op"], json!("remove"));
    assert_eq!(json_of(&PatchOp::Move { occ: Occ::new("o1"), to: 0 })["op"], json!("move"));
    assert_eq!(json_of(&PatchOp::Update { occ: Occ::new("o1"), value: json!(1) })["op"], json!("update"));
    assert_eq!(json_of(&PatchOp::Rekey { occ: Occ::new("o1"), key: json!("k") })["op"], json!("rekey"));
}

#[test]
fn every_patch_op_round_trips() {
    round_trip(PatchOp::Insert { at: 0, occ: Occ::new("a"), value: json!(null) });
    round_trip(PatchOp::Remove { occ: Occ::new("b") });
    round_trip(PatchOp::Move { occ: Occ::new("c"), to: 7 });
    round_trip(PatchOp::Update { occ: Occ::new("d"), value: json!({ "x": [1, 2, 3] }) });
    round_trip(PatchOp::Rekey { occ: Occ::new("e"), key: json!("newkey") });
}

#[test]
fn every_downstream_frame_round_trips() {
    let rows = vec![WireRow::new(Occ::new("a"), json!(1)), WireRow::new(Occ::new("b"), json!(2))];
    round_trip(Downstream::Init { sub: Sub::new("s"), rows });
    round_trip(Downstream::Scalar { sub: Sub::new("s"), value: json!(42) });
    round_trip(Downstream::Patch {
        sub: Sub::new("s"),
        ops: vec![PatchOp::Remove { occ: Occ::new("a") }],
    });
    round_trip(Downstream::Close { sub: Sub::new("s"), reason: CloseReason::Unauthorized });
    round_trip(Downstream::Frontier);
    round_trip(Downstream::Reset { reason: ResetReason::UnknownConnection });
    round_trip(Downstream::Fault { code: FaultCode::BadToken, message: "forged".into() });

    // The frontier-only frame is exactly the tag, nothing more.
    assert_eq!(json_of(&Downstream::Frontier), json!({ "type": "frontier" }));
    assert_eq!(json_of(&Downstream::Reset { reason: ResetReason::Overflow })["reason"], json!("overflow"));
}

#[test]
fn every_upstream_frame_round_trips() {
    round_trip(Upstream::Hello { auth: None, context: None });
    round_trip(Upstream::Hello { auth: Some(json!({ "authenticator": "token" })), context: Some(json!("ctx")) });
    round_trip(Upstream::Manifest);
    round_trip(Upstream::View {
        sub: Sub::new("s"),
        address: "public.tasks.index".into(),
        params: None,
        window: None,
        auth: None,
        context: None,
    });
    round_trip(Upstream::View {
        sub: Sub::new("s"),
        address: "public.open".into(),
        params: Some(json!({ "q": "x" })),
        window: Some(WireWindow { size: 3, anchor: WireAnchor::At { occ: Occ::new("o9") }, slide: true }),
        auth: Some(json!({ "authenticator": "token" })),
        context: Some(json!("member")),
    });
    round_trip(Upstream::Unsubscribe { sub: Sub::new("s") });
    round_trip(Upstream::Call { address: "public.tasks.add".into(), args: json!({ "title": "t" }), auth: None, context: None });
    round_trip(Upstream::Fetch { address: "public.tasks".into(), params: Some(json!({ "id": "t1" })) });
    round_trip(Upstream::Operation { operation: liasse_wire::OperationId::new("op-1") });

    // Minimal call is exactly the plan's `call{address,args}`.
    let call = Upstream::Call { address: "a".into(), args: json!([]), auth: None, context: None };
    assert_eq!(json_of(&call), json!({ "type": "call", "address": "a", "args": [] }));
    assert_eq!(json_of(&Upstream::Manifest), json!({ "type": "manifest" }));
}

#[test]
fn window_anchor_defaults_to_first_when_absent() {
    // A window with no `anchor`/`slide` decodes to the first-window default.
    let window: WireWindow = decode(r#"{ "size": 5 }"#).expect("bare window decodes");
    assert_eq!(window, WireWindow { size: 5, anchor: WireAnchor::First, slide: false });
    // The anchor is kind-tagged so an occurrence never collides with a keyword.
    assert_eq!(json_of(&WireAnchor::Last), json!({ "kind": "last" }));
    assert_eq!(json_of(&WireAnchor::At { occ: Occ::new("o") }), json!({ "kind": "at", "occ": "o" }));
}

#[test]
fn every_outcome_frame_round_trips() {
    round_trip(Outcome::Committed { frontier: Ft::new("f2"), commit: Ft::new("f2"), response: Some(json!({ "id": "x" })) });
    round_trip(Outcome::Committed { frontier: Ft::new("f2"), commit: Ft::new("f1"), response: None });
    round_trip(Outcome::Unchanged { frontier: Ft::new("f1"), response: None });
    round_trip(Outcome::Rejected { code: Code::new("duplicate-key"), message: "key `a` exists".into() });
    round_trip(Outcome::Denied { code: Code::new("unresolved"), message: "refused".into() });
    round_trip(Outcome::Failed { code: FailedCode::AbsentAnchor });
    round_trip(Outcome::Failed { code: FailedCode::ScalarView });
    round_trip(Outcome::Unknown);

    assert_eq!(json_of(&Outcome::Unknown), json!({ "status": "unknown" }));
    assert_eq!(json_of(&Outcome::Failed { code: FailedCode::AbsentAnchor })["code"], json!("absent-anchor"));
    assert_eq!(json_of(&Outcome::Failed { code: FailedCode::ScalarView })["code"], json!("scalar-view"));
}

#[test]
fn opaque_tokens_are_bare_strings_on_the_wire() {
    assert_eq!(json_of(&Occ::new("occ-1")), json!("occ-1"));
    assert_eq!(json_of(&Ft::new("ft-1")), json!("ft-1"));
    assert_eq!(json_of(&ConnectionToken::new("conn")), json!("conn"));
    let occ: Occ = decode(r#""back""#).expect("decode token");
    assert_eq!(occ.as_str(), "back");
}
