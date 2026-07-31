//! Closed declaration objects (SPEC.md §2.5): "Unknown members in a declaration
//! object are invalid unless that declaration explicitly accepts
//! application-defined member names."
//!
//! Each object-form node builder reads the members its form defines. What it
//! does not read must be REJECTED BY NAME, never dropped: a builder that keeps
//! the marker and discards its siblings makes the package load meaning something
//! other than what it says. The regression that motivated this file is the
//! `$set` one — `{ "$set": "text", "$check": [...] }` built the set and threw the
//! membership constraint away, in a keyed row and at the model root alike — but
//! the same hole sat under `$view`, `$ref`, an inline `$enum`, `$like`, and a
//! source-backed `$bucket` collection.
//!
//! Every rejection test is paired with a CONTROL that the form's own vocabulary
//! still loads, so none of these can pass by rejecting the whole form.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;
use liasse_model::code;

fn model(body: &str) -> String {
    format!("{{ \"$liasse\": 1, \"$app\": \"t.cd@1.0.0\", \"$model\": {body} }}")
}

/// Every rejection in this file carries the closed-declaration code, names the
/// offending member in its message, and points at that member's own bytes.
fn assert_names_member(built: &common::Built, member: &str) {
    assert!(
        built.has_code(code::UNKNOWN_MEMBER),
        "expected {} naming `{member}`, got:\n{}",
        code::UNKNOWN_MEMBER,
        built.rendered()
    );
    assert!(
        built.rendered().contains(&format!("`{member}` may not accompany")),
        "the diagnostic must name `{member}`, got:\n{}",
        built.rendered()
    );
    assert!(built.points_at(member), "the span must cover `{member}`:\n{}", built.rendered());
    assert!(built.has_hint(), "the rejection must hint at the supported alternative");
}

// ---------------------------------------------------------------------------
// `$set` (§5.5) — the reported defect. Accepted: `$set`.
// ---------------------------------------------------------------------------

/// §2.5/§5.5 — a `$check` beside a `$set` in a KEYED ROW is rejected by name.
/// Before this it loaded and the check simply did not exist.
#[test]
fn set_check_in_row_rejected() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text",
              "flags": { "$set": "text", "$check": ["size(.flags) <= 3", "at most three"] } } }"#,
    ));
    assert_names_member(&built, "$check");
}

/// §2.5/§5.5 — the same at the MODEL ROOT: a root singleton set is the other
/// position the defect reproduced in.
#[test]
fn set_check_at_root_rejected() {
    let built = build(&model(
        r#"{ "tags": { "$set": "text", "$check": ["size(.tags) <= 3", "at most three"] } }"#,
    ));
    assert_names_member(&built, "$check");
}

/// §2.5 — an application-named member beside a `$set` is equally invalid: §5.5
/// gives the form no application-defined member names.
#[test]
fn set_application_named_member_rejected() {
    let built = build(&model(r#"{ "tags": { "$set": "text", "limit": "int" } }"#));
    assert_names_member(&built, "limit");
}

/// §2.5 — a `$normalize` beside a `$set` is a refinement the set node has
/// nowhere to store, so it is named rather than discarded.
#[test]
fn set_normalize_rejected() {
    let built = build(&model(r#"{ "tags": { "$set": "text", "$normalize": "= lower(.)" } }"#));
    assert_names_member(&built, "$normalize");
}

/// CONTROL — the supported spelling the `$set` hint points at: the membership
/// constraint lives on the CONTAINING shape's `$check` (§5.10), beside the set.
/// This is what must keep loading, or the rejection above would be a regression
/// rather than a fix.
#[test]
fn set_with_containing_shape_check_loads() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text",
              "flags": { "$set": "text" },
              "$check": [["size(.flags) <= 3", "at most three flags"]] } }"#,
    ));
    built.expect_ok();
}

/// CONTROL — a bare set, and a set of refs carrying the `$on_delete` its element
/// form does accept (§5.6/§21.1), both still load.
#[test]
fn plain_sets_load() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text",
              "tags": { "$set": "text" },
              "kinds": { "$set": { "$enum": ["a", "b"] } },
              "peers": { "$set": { "$ref": "/docs", "$on_delete": "cascade" } } } }"#,
    ));
    built.expect_ok();
}

// ---------------------------------------------------------------------------
// `$view` (§7) — accepted: `$view`.
// ---------------------------------------------------------------------------

/// §2.5/§7.3 — a view's ordering is written inside the `$view` expression
/// (C.7); a sibling `$sort` member reached nothing and is now named.
#[test]
fn view_sibling_sort_rejected() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text" },
            "listing": { "$view": ".docs", "$sort": ["id"] } }"#,
    ));
    assert_names_member(&built, "$sort");
}

/// §2.5 — an application-named member beside a `$view`.
#[test]
fn view_application_named_member_rejected() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text" },
            "listing": { "$view": ".docs", "caption": "text" } }"#,
    ));
    assert_names_member(&built, "caption");
}

/// CONTROL — the same ordering declared where §7.3/C.7 puts it loads.
#[test]
fn view_with_inline_sort_loads() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text" },
            "listing": { "$view": ".docs { id, $sort: [-id] }" } }"#,
    ));
    built.expect_ok();
}

// ---------------------------------------------------------------------------
// `$ref` (§5.6, §21.1) — accepted: `$ref`, `$optional`, `$on_delete`.
// ---------------------------------------------------------------------------

/// §2.5/§5.6 — a `$check` beside a `$ref` never became a check; it is now named.
#[test]
fn ref_check_rejected() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text",
              "parent": { "$ref": "/docs", "$check": "true" } } }"#,
    ));
    assert_names_member(&built, "$check");
}

/// §2.5 — an application-named member beside a `$ref`.
#[test]
fn ref_application_named_member_rejected() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text",
              "parent": { "$ref": "/docs", "label": "text" } } }"#,
    ));
    assert_names_member(&built, "label");
}

/// §2.5/§5.5 — the SET-ELEMENT ref position is a declaration object too, and no
/// dispatcher judges its markers, so it is closed on the same vocabulary.
#[test]
fn set_of_refs_element_member_rejected() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text",
              "peers": { "$set": { "$ref": "/docs", "$default": "x" } } } }"#,
    ));
    assert_names_member(&built, "$default");
}

/// CONTROL — the three members the ref form does read all load together.
#[test]
fn ref_with_optional_and_on_delete_loads() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text",
              "parent": { "$ref": "/docs", "$optional": true, "$on_delete": "cascade" } } }"#,
    ));
    built.expect_ok();
}

// ---------------------------------------------------------------------------
// inline `$enum` (§5.9) — accepted: `$enum`.
// ---------------------------------------------------------------------------

/// §2.5/§5.9 — a member beside a bare inline `$enum` that is not one of the
/// §5.1 expanded-field refinements is outside both vocabularies.
#[test]
fn inline_enum_foreign_member_rejected() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text",
              "status": { "$enum": ["draft", "active"], "$max_bytes": 8 } } }"#,
    ));
    assert_names_member(&built, "$max_bytes");
}

/// §2.5/§5.9 — an inline enum in a TYPE position (an expanded field's `$type`)
/// is closed the same way; nothing dispatched its markers there either.
#[test]
fn inline_enum_in_type_position_foreign_member_rejected() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text",
              "status": { "$type": { "$enum": ["draft", "active"], "extra": "text" },
                          "$default": "draft" } } }"#,
    ));
    assert_names_member(&built, "extra");
}

/// CONTROL — a bare inline enum, and one carrying the §5.1 refinements that
/// make it an expanded field, both load.
#[test]
fn inline_enums_load() {
    let built = build(&model(
        r#"{ "docs": { "$key": "id", "id": "text",
              "bare": { "$enum": ["a", "b"] },
              "refined": { "$enum": ["draft", "active"], "$default": "draft",
                           "$optional": true } } }"#,
    ));
    built.expect_ok();
}

// ---------------------------------------------------------------------------
// `$like` (§5.8) — accepted: `$like`.
// ---------------------------------------------------------------------------

/// §2.5/§5.8 — a `$like` field adopts the named shape whole; a sibling
/// refinement reached nothing.
#[test]
fn like_sibling_member_rejected() {
    let built = build(&model(
        r#"{ "nodes": { "$key": "id", "id": "text",
              "children": { "$like": "^", "$optional": true } } }"#,
    ));
    assert_names_member(&built, "$optional");
}

/// CONTROL — the bare positional recursion loads.
#[test]
fn like_alone_loads() {
    let built = build(&model(
        r#"{ "nodes": { "$key": "id", "id": "text", "children": { "$like": "^" } } }"#,
    ));
    built.expect_ok();
}

// ---------------------------------------------------------------------------
// `$keyring` (§17.1, C.16) — accepted: `$keyring`. Already fail-closed before
// this change; pinned here so the shared helper cannot silently lose it.
// ---------------------------------------------------------------------------

/// §2.5/§17.1 — nothing accompanies a `$keyring` declaration.
#[test]
fn keyring_sibling_member_rejected() {
    let built = build(&model(
        r#"{ "session_keys": { "$keyring": { "$provider": "session-hsm", "$algorithm": "Ed25519" },
              "$check": "true" } }"#,
    ));
    assert_names_member(&built, "$check");
}

// ---------------------------------------------------------------------------
// source-backed `$bucket` collection (§14.4–§14.6) — accepted: `$bucket`,
// `$key`, and the application-named output fields (§2.5's explicit exception).
// ---------------------------------------------------------------------------

/// §2.5/§14.4 — a source-backed bucket's rows are derived and read-only, so a
/// `$roles` on the collection acted on nothing; it is now named instead of
/// dropped on the floor.
#[test]
fn source_bucket_reserved_sibling_rejected() {
    let built = build(&model(
        r#"{ "subs": { "$key": "id", "id": "uuid = uuid()", "starts_at": "timestamp" },
            "periods": {
              "$bucket": { "$source": ".subs", "$from": "$source.starts_at" },
              "$roles": {},
              "label": "= 'p'"
            } }"#,
    ));
    assert_names_member(&built, "$roles");
}

/// CONTROL — the whole vocabulary the source-bucket phase reads (`$bucket`, a
/// §14.6 custom `$key`, and application-named output fields) loads.
#[test]
fn source_bucket_with_key_and_output_fields_loads() {
    let built = build(&model(
        r#"{ "subs": { "$key": "id", "id": "uuid = uuid()",
              "external_id": "text", "starts_at": "timestamp", "ends_at": "timestamp? = none" },
            "periods": {
              "$bucket": { "$source": ".subs", "$from": "$source.starts_at",
                           "$until": "$source.ends_at" },
              "$key": ["$source.external_id", "$from"],
              "label": "= $source.external_id"
            } }"#,
    ));
    built.expect_ok();
}

// ---------------------------------------------------------------------------
// No doubled diagnostics: object_node's marker guards own the members they
// already reject by name, so the closed-vocabulary check must not repeat them.
// ---------------------------------------------------------------------------

/// Annex C.2 — two kind markers are one mistake and get one diagnostic; the
/// §2.5 check skips what the marker guard already named.
#[test]
fn conflicting_markers_are_not_reported_twice() {
    let built = build(&model(r#"{ "thing": { "$set": "text", "$view": ".x" } }"#));
    let rendered = built.rendered();
    assert!(built.has_code(code::SHAPE), "expected the marker conflict:\n{rendered}");
    assert!(
        rendered.contains("conflicting shape markers") && rendered.contains("`$view`"),
        "the conflict diagnostic must name `$view`:\n{rendered}"
    );
    assert!(
        !built.has_code(code::UNKNOWN_MEMBER),
        "the marker guard already named `$view`; it must not be reported again:\n{rendered}"
    );
}

/// §5.4/C.2 — the same for a misplaced `$value`, whose own guard names it.
#[test]
fn misplaced_value_marker_is_not_reported_twice() {
    let built = build(&model(r#"{ "thing": { "$set": "text", "$value": "text" } }"#));
    let rendered = built.rendered();
    assert!(built.has_code(code::SHAPE), "expected the `$value` guard:\n{rendered}");
    assert!(
        !built.has_code(code::UNKNOWN_MEMBER),
        "the `$value` guard already named it; it must not be reported again:\n{rendered}"
    );
}
