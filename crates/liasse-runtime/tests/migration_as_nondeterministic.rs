#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §20.1 red case: a local `$from`/`$as` migration transform MUST use
//! deterministic pure functions, exactly like the `$migrations` program does.
//!
//! §20.1 states a migration "MUST use deterministic pure functions", and §20.2's
//! reversibility invariant `$back($as(x)) == x` (verified "for every actual
//! migrated value x") is only well-defined when `$as` is a deterministic function
//! of its input `.`. §16.3 classifies `uuid()`/`now()` as `generated` ("may use
//! randomness, clocks"), and SPEC's position policy (line ~1145/1147: "views ...
//! use pure functions only" / "The checker rejects an effect class used in the
//! wrong position while loading the package") bars a generated function from a
//! pure position.
//!
//! The implementation already enforces this in the two sibling positions:
//!   * `uuid()`/`now()` in a `$migrations` program are rejected (`M-MIGRATE`);
//!   * `uuid()` in a `$view` / `$check` is rejected at load.
//!
//! But a `$as` transform is admitted with `uuid()`/`now()`, so a fresh random
//! value unrelated to the source is baked into committed migrated state.

mod support;

use liasse_runtime::Value;
use liasse_value::Text;
use support::{generator, load};

/// Source package: one `users` row, `name = "Ann"`.
const V1: &str = r#"{
  "$liasse": 1
  "$app": "example.nd@1.0.0"
  "$model": {
    "users": { "$key": "id", "id": "text", "name": "text" }
    "all": { "$view": ".users { id, name }" }
  }
  "$data": { "users": { "u1": { "name": "Ann" } } }
}"#;

/// Target: a new `token: uuid` field migrated with `$as: "uuid()"`. The `$as`
/// ignores its input `.` and calls the non-deterministic `uuid()`.
const V2_UUID_AS: &str = r#"{
  "$liasse": 1
  "$app": "example.nd@2.0.0"
  "$model": {
    "users": {
      "$key": "id"
      "id": "text"
      "name": "text"
      "token": { "$type": "uuid", "$from": "name", "$as": "uuid()" }
    }
    "all": { "$view": ".users { id, name, token }" }
  }
}"#;

/// Target: a new `at: timestamp` field migrated with `$as: "now()"`.
const V2_NOW_AS: &str = r#"{
  "$liasse": 1
  "$app": "example.nd@2.0.0"
  "$model": {
    "users": {
      "$key": "id"
      "id": "text"
      "name": "text"
      "at": { "$type": "timestamp", "$from": "name", "$as": "now()" }
    }
    "all": { "$view": ".users { id, name, at }" }
  }
}"#;

/// Deterministic control: a new `handle: text` field migrated with the pure
/// `$as: "string.upper(.)"`, a deterministic function of its input `.`. It MUST
/// still commit — the §20.1 gate bars only generated calls, never legitimate
/// pure transforms (`string.*`, `base64.*`, arithmetic, field access, …).
const V2_UPPER_AS: &str = r#"{
  "$liasse": 1
  "$app": "example.nd@2.0.0"
  "$model": {
    "users": {
      "$key": "id"
      "id": "text"
      "name": "text"
      "handle": { "$type": "text", "$from": "name", "$as": "string.upper(.)" }
    }
    "all": { "$view": ".users { id, name, handle }" }
  }
}"#;

/// §20.1: a `$as` transform calling the non-deterministic `uuid()` must be
/// rejected — as the identical call already is inside a `$migrations` program and
/// inside any pure position (`$view`/`$check`). The implementation instead admits
/// the migration and commits a random UUID that is not a function of the source.
#[test]
fn as_transform_calling_uuid_must_reject() {
    let mut engine = load("nd-uuid", V1);
    let mut generator = generator();
    match engine.update(V2_UUID_AS, &mut generator) {
        Err(_) => { /* conformant: the non-deterministic transform is refused */ }
        Ok(report) => {
            let view = engine.view_at_head("all").expect("view").expect("declared");
            let token = view.rows()[0].field("token").cloned();
            panic!(
                "§20.1 requires a migration transform to use deterministic pure functions, and §20.2's \
                 `$back($as(x)) == x` presumes `$as` is a function of its input; `$as: \"uuid()\"` is neither. \
                 The impl rejects the same `uuid()` in a `$migrations` program and in a `$view`/`$check`, \
                 but here the migration COMMITTED ({report:?}) with a fresh non-source-derived token {token:?}",
            );
        }
    }
    let _ = Value::None;
}

/// The `now()` sibling: `$as: "now()"` is likewise non-deterministic (a clock
/// read, `generated` per §16.3) and must be rejected in a migration transform.
#[test]
fn as_transform_calling_now_must_reject() {
    let mut engine = load("nd-now", V1);
    let mut generator = generator();
    if let Ok(report) = engine.update(V2_NOW_AS, &mut generator) {
        panic!(
            "§20.1: a `$as: \"now()\"` migration transform must be rejected as non-deterministic \
             (it is rejected inside a `$migrations` program), but the migration committed: {report:?}",
        );
    }
}

/// Control: the gate must NOT over-reject. A deterministic pure `$as`
/// (`string.upper(.)`, a function of the source `name`) still commits, and the
/// migrated value is the source-derived `"ANN"` — proving only generated calls
/// are barred, not legitimate deterministic transforms.
#[test]
fn as_transform_deterministic_still_commits() {
    let mut engine = load("nd-upper", V1);
    let mut generator = generator();
    engine
        .update(V2_UPPER_AS, &mut generator)
        .expect("a deterministic `string.upper(.)` transform must commit");
    let view = engine.view_at_head("all").expect("view").expect("declared");
    assert_eq!(
        view.rows()[0].field("handle"),
        Some(&text("ANN")),
        "the deterministic transform is applied to the source value `Ann`",
    );
}

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}
