//! State-model rejections (§5): each asserts the outcome and the diagnostic's
//! code, span, and hint.

mod common;

use common::build;
use liasse_model::code;

fn model(body: &str) -> String {
    format!("{{ \"$liasse\": 1, \"$app\": \"t.st@1.0.0\", \"$model\": {body} }}")
}

#[test]
fn declaration_name_leading_underscore_rejected() {
    // §2.5: a declaration name must begin with an ASCII letter.
    let built = build(&model("{ \"_hidden\": \"text\" }"));
    assert!(built.has_code(code::NAME_GRAMMAR));
    assert!(built.points_at("_hidden"));
}

#[test]
fn declaration_name_non_ascii_rejected() {
    let built = build(&model("{ \"naïve\": \"text\" }"));
    assert!(built.has_code(code::NAME_GRAMMAR));
}

#[test]
fn unknown_reserved_member_rejected() {
    // §2.5: a `$`-prefixed member that is not a declaration is invalid.
    let built = build(&model(
        "{ \"things\": { \"$key\": \"id\", \"id\": \"text\", \"$bogus\": true } }",
    ));
    assert!(built.has_code(code::RESERVED_MEMBER));
    assert!(built.points_at("$bogus"));
}

#[test]
fn key_names_undeclared_field_rejected() {
    // §5.4: `$key` names a declared field.
    let built = build(&model("{ \"things\": { \"$key\": \"id\", \"name\": \"text\" } }"));
    assert!(built.has_code(code::KEY));
    assert!(built.points_at("id"));
}

#[test]
fn optional_key_field_rejected() {
    // A.8: optional types are excluded from row keys.
    let built = build(&model(
        "{ \"items\": { \"$key\": \"id\", \"id\": \"text?\", \"name\": \"text\" } }",
    ));
    assert!(built.has_code(code::KEY));
    assert!(built.has_hint());
}

#[test]
fn non_key_eligible_field_rejected() {
    // A.8: json is not a key-eligible type.
    let built = build(&model(
        "{ \"items\": { \"$key\": \"blob_id\", \"blob_id\": \"json\" } }",
    ));
    assert!(built.has_code(code::KEY));
}

#[test]
fn enum_duplicate_labels_rejected() {
    // §5.9: enum labels must be distinct.
    let built = build(&model(
        "{ \"things\": { \"$key\": \"id\", \"id\": \"text\", \"status\": { \"$enum\": [\"draft\", \"active\", \"draft\"] } } }",
    ));
    assert!(built.has_code(code::ENUM));
    assert!(built.has_hint());
}

#[test]
fn ref_to_missing_collection_rejected() {
    // §5.6: a ref target must be a declared collection.
    let built = build(&model(
        "{ \"contacts\": { \"$key\": \"id\", \"id\": \"text\", \"company\": { \"$ref\": \"/companies\" } } }",
    ));
    assert!(built.has_code(code::REF));
    assert!(built.points_at("$ref"));
    assert!(built.has_hint());
}

#[test]
fn ref_to_existing_collection_accepted() {
    let built = build(&model(
        "{ \"companies\": { \"$key\": \"id\", \"id\": \"text\" }, \"contacts\": { \"$key\": \"id\", \"id\": \"text\", \"company\": { \"$ref\": \"/companies\" } } }",
    ));
    built.expect_ok();
}

#[test]
fn ref_key_field_accepted() {
    // §A.9/§10.3: a `$key` naming a required `$ref` field is valid — the ref's
    // key type is the target collection's (already key-eligible) key type. This
    // is the idiomatic scoped-membership shape (`$key: "account"` over a
    // `{ $ref: "/accounts" }` field).
    let built = build(&model(
        "{ \"accounts\": { \"$key\": \"id\", \"id\": \"text\" }, \"members\": { \"$key\": \"account\", \"account\": { \"$ref\": \"/accounts\" }, \"admin\": \"bool = false\" } }",
    ));
    built.expect_ok();
}

#[test]
fn optional_ref_key_field_rejected() {
    // §A.8: optional types are excluded from row keys, so an optional ref is
    // not a valid `$key` field.
    let built = build(&model(
        "{ \"accounts\": { \"$key\": \"id\", \"id\": \"text\" }, \"members\": { \"$key\": \"account\", \"account\": { \"$ref\": \"/accounts\", \"$optional\": true } } }",
    ));
    assert!(built.has_code(code::KEY));
}

#[test]
fn default_dependency_cycle_rejected() {
    // §5.1: the default dependency graph must be acyclic.
    let built = build(&model(
        "{ \"items\": { \"$key\": \"id\", \"id\": \"text\", \"a\": \"int = .b + 1\", \"b\": \"int = .a + 1\" } }",
    ));
    assert!(built.has_code(code::CYCLE));
    assert!(built.has_hint());
}

#[test]
fn unknown_type_name_rejected() {
    let built = build(&model("{ \"x\": \"txt\" }"));
    assert!(built.has_code(code::TYPE));
}

#[test]
fn row_check_typed_against_row_accepted() {
    // §5.10: a row `$check` sees the prospective row as `.`.
    let built = build(&model(
        "{ \"periods\": { \"$key\": \"id\", \"id\": \"text\", \"starts_at\": \"timestamp\", \"ends_at\": \"timestamp\", \"$check\": [\".ends_at > .starts_at\", \"The period must advance\"] } }",
    ));
    built.expect_ok();
}
