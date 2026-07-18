#![cfg(target_arch = "wasm32")]
//! The wasm-bindgen surface, run under `wasm-pack test --node`. These mirror the
//! native core suite but through the JS boundary: frames arrive as strings, results
//! come back as `JsValue`, and a malformed frame must be a JS error, never a panic
//! (the no-panic gate at the boundary, AGENTS.md). Expected states are deduced from
//! §12.2 by hand, never from the client's own output.

use js_sys::JSON;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

use liasse_connect_wasm::{OperationHandle, WireClient, encode_view};

/// The canonical JSON text of a `JsValue`, for an externally deducible comparison.
fn json_text(value: &JsValue) -> String {
    String::from(JSON::stringify(value).expect("stringify"))
}

#[wasm_bindgen_test]
fn apply_frame_folds_init_then_patch_through_the_js_boundary() {
    let mut client = WireClient::new();

    let init = client
        .apply_frame(r#"{"type":"init","sub":"s","rows":[{"id":"a","value":1}]}"#, "f0")
        .expect("init frame applies");
    assert!(json_text(&init).contains("\"kind\":\"init\""), "{}", json_text(&init));

    client
        .apply_frame(r#"{"type":"patch","sub":"s","ops":[{"op":"insert","at":1,"id":"b","value":2}]}"#, "f1")
        .expect("patch frame applies");

    assert_eq!(client.frontier("s"), Some("f1".to_owned()));
    assert_eq!(client.connection_frontier(), Some("f1".to_owned()));
    assert!(!client.is_closed("s"));
    assert_eq!(client.subs(), vec!["s".to_owned()]);

    let rows = client.rows("s").expect("rows marshal");
    assert_eq!(json_text(&rows), r#"[{"id":"a","value":1},{"id":"b","value":2}]"#);
}

#[wasm_bindgen_test]
fn a_scalar_frame_is_readable_as_a_value() {
    let mut client = WireClient::new();
    client.apply_frame(r#"{"type":"scalar","sub":"c","value":41}"#, "f0").expect("scalar frame applies");
    assert_eq!(json_text(&client.scalar("c").expect("scalar marshal")), "41");
}

#[wasm_bindgen_test]
fn a_malformed_or_orphan_frame_is_a_js_error_not_a_panic() {
    let mut client = WireClient::new();
    assert!(client.apply_frame("not json", "f0").is_err());
    assert!(client.apply_frame(r#"{"type":"nope"}"#, "f0").is_err());
    // a patch for a subscription that was never opened is refused, not invented.
    assert!(client.apply_frame(r#"{"type":"patch","sub":"ghost","ops":[]}"#, "f0").is_err());
}

#[wasm_bindgen_test]
fn a_close_frame_terminates_the_subscription() {
    let mut client = WireClient::new();
    client.apply_frame(r#"{"type":"init","sub":"s","rows":[]}"#, "f0").expect("init");
    client.apply_frame(r#"{"type":"close","sub":"s","reason":"unsubscribed"}"#, "f1").expect("close");
    assert!(client.is_closed("s"));
    assert_eq!(client.close_reason("s"), Some("unsubscribed".to_owned()));
}

#[wasm_bindgen_test]
fn encode_view_marshals_a_js_params_object_into_a_wire_body() {
    let params = JSON::parse(r#"{"q":"x"}"#).expect("params object");
    let body = encode_view("s", "public.tasks", params, JsValue::NULL, JsValue::NULL, JsValue::NULL)
        .expect("encode view");
    assert!(body.contains(r#""type":"view""#), "{body}");
    assert!(body.contains(r#""sub":"s""#), "{body}");
    assert!(body.contains(r#""q":"x""#), "{body}");
}

#[wasm_bindgen_test]
fn an_operation_handle_carries_a_client_seeded_id() {
    let handle = OperationHandle::new("op-7".to_owned()).expect("non-empty id");
    assert_eq!(handle.id(), "op-7");
    let body = handle.status_frame().expect("status frame");
    assert!(body.contains(r#""type":"operation""#), "{body}");
    assert!(body.contains(r#""operation":"op-7""#), "{body}");
    // an empty operation capability is refused at construction.
    assert!(OperationHandle::new(String::new()).is_err());
}
