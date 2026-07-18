#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM regression — the runtime REJECTS an `optional<T>` inside a `set`
//! element / `map` value shape at `Engine::load`, so a `none` can never reach a
//! set member or a map value through an authored type.
//!
//! SPEC.md line 489: "the member shape of a set is never `optional<T>`." A.1 line
//! 4400: "A map never stores a `none` value; absence is the key not being
//! present." Both make an `optional<T>` set-element / map-value shape a STATIC
//! error. Before the fix all three spellings below LOADED, so decode / mutation
//! then stored a `none` (see redteam_map_none_absence, redteam_map_none_value_decode);
//! now each is rejected as the definition fails static validation.
//!
//! This asserts the rejection propagates through the whole `Engine::load` path;
//! the diagnostic wording is pinned at the model layer (liasse-model
//! `redteam_optional_container_inner`), where the message is accessible.

mod support;

use support::store;

use liasse_runtime::Engine;

fn loads(instance: &str, def: &str) -> bool {
    let mut generator = support::generator();
    Engine::load(store(instance), def, &mut generator).is_ok()
}

const MAP_OPT_VALUE: &str = r#"{
  "$liasse": 1, "$app": "probe.map.optval@1.0.0",
  "$model": { "docs": { "$key": "id", "id": "text", "meta": "map<text, optional<text>>" } }
}"#;

const SET_OPT_ELEM_STRING: &str = r#"{
  "$liasse": 1, "$app": "probe.set.optelem.str@1.0.0",
  "$model": { "docs": { "$key": "id", "id": "text", "tags": "set<optional<text>>" } }
}"#;

const SET_OPT_ELEM_INLINE: &str = r#"{
  "$liasse": 1, "$app": "probe.set.optelem.inline@1.0.0",
  "$model": { "docs": { "$key": "id", "id": "text", "tags": { "$set": "optional<text>" } } }
}"#;

#[test]
fn optional_set_element_and_map_value_shapes_are_rejected_at_load() {
    // §5.5 line 489 / A.1 line 4400: all three spellings of an `optional<T>`
    // set-element / map-value shape are a static error.
    let mut wrongly_accepted = Vec::new();
    if loads("probe-map-optval", MAP_OPT_VALUE) {
        wrongly_accepted.push("map<text, optional<text>>");
    }
    if loads("probe-set-optstr", SET_OPT_ELEM_STRING) {
        wrongly_accepted.push("set<optional<text>>");
    }
    if loads("probe-set-optinline", SET_OPT_ELEM_INLINE) {
        wrongly_accepted.push("{ $set: optional<text> }");
    }
    assert!(
        wrongly_accepted.is_empty(),
        "SPEC §5.5 line 489 / A.1 line 4400: an `optional<T>` set-element / map-value shape must be a \
         static error, but the runtime accepted: {wrongly_accepted:?}"
    );
}
