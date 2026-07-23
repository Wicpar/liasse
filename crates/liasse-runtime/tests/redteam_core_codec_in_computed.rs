#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §16.1/§16.5 probe: the core codec namespaces (`base64`, `hex`, and the
//! `string` byte codecs) are listed in §16.1 as *core* namespaces, available
//! without a `$requires` declaration. §16.5 permits the built-in namespaces
//! (§16.1) in EVERY database-evaluated position — a `$view`, a `$check`, a
//! `$normalize`, a computed value (§5.2), a field default, etc. — not only the
//! §20 migration transform.
//!
//! This is the exact assertion the corpus case
//! `16-host-namespaces/core-namespaces-load-without-requires` makes with
//! `banner: { $view: "base64.encode(string.bytes('liasse')) }"`, which is
//! skip-listed at the CORE static-model layer (no host descriptors) and skipped
//! as a static case by the runtime testkit — so nothing verifies it end to end.
//!
//! Hand-derived expectation: `base64.encode(string.bytes("liasse"))` is the
//! canonical base64 of the UTF-8 bytes of "liasse". "liasse" is
//! `6c 69 61 73 73 65` (6 bytes) -> base64 "bGlhc3Nl".

mod support;

use liasse_runtime::{CallRequest, Value};
use liasse_value::Text;
use serde_json::json;
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// A package whose computed value uses the core `base64`/`string` codecs in a
/// database-evaluated position (§5.2 computed value), with no `$requires`.
const CODEC_APP: &str = r#"{
  "$liasse": 1,
  "$app": "example.codec@1.0.0",
  "$model": {
    "notes": {
      "$key": "id",
      "id": "text",
      "name": "text",
      "encoded": "= base64.encode(string.bytes(.name))"
    },
    "notes_view": { "$view": ".notes { id, encoded }" },
    "$mut": {
      "add({ id: text, name: text })": [
        "row = .notes + { id: @id, name: @name }",
        "return row { id, encoded }"
      ]
    }
  }
}"#;

#[test]
fn core_codec_available_in_computed_value() {
    let mut engine = load("codec", CODEC_APP);
    let mut generator = generator();

    let outcome = engine
        .call(
            &CallRequest::new("add").arg("id", text("n1")).arg("name", text("liasse")),
            &mut generator,
        )
        .expect("call succeeds");
    let response = outcome.response().expect("add returns a row").to_wire();
    assert_eq!(response, json!({ "id": "n1", "encoded": "bGlhc3Nl" }));

    let view = engine.view_at_head("notes_view").expect("view").expect("declared");
    assert_eq!(view.rows()[0].field("encoded"), Some(&text("bGlhc3Nl")));
}
