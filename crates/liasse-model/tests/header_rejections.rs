//! Header rejections (§4, §2.5, Annex E): each asserts the outcome *and* the
//! diagnostic's code, span, and hint quality.

mod common;

use common::build;
use liasse_model::code;

fn app(model_and_extra: &str) -> String {
    format!(
        "{{ \"$liasse\": 1, \"$app\": \"t.case@1.0.0\", {model_and_extra} }}"
    )
}

#[test]
fn unknown_top_level_member_rejected() {
    // §2.5 / Annex C.1: the top-level object accepts no application-defined
    // members.
    let built = build(&app("\"$model\": { \"note\": \"text\" }, \"custom_notes\": {}"));
    assert!(built.has_code(code::UNKNOWN_MEMBER));
    assert!(built.points_at("custom_notes"));
    assert!(built.has_hint());
}

#[test]
fn module_member_on_application_rejected() {
    // §4.1: `$config` is a module-only member.
    let built = build(&app("\"$config\": {}, \"$model\": { \"note\": \"text\" }"));
    assert!(built.has_code(code::UNKNOWN_MEMBER));
    assert!(built.points_at("$config"));
}

#[test]
fn app_and_module_both_declared_rejected() {
    let built = build(
        "{ \"$liasse\": 1, \"$app\": \"t.a@1.0.0\", \"$module\": \"t.b@1.0.0\", \"$model\": {} }",
    );
    assert!(built.has_code(code::HEADER));
    assert!(built.has_hint());
}

#[test]
fn unsupported_liasse_generation_rejected() {
    // §4.1: an unsupported generation is rejected before other declarations.
    let built = build("{ \"$liasse\": 2, \"$app\": \"t.future@1.0.0\", \"$model\": {} }");
    assert!(built.has_code(code::LANGUAGE));
    assert!(built.points_at("2"));
}

#[test]
fn missing_liasse_version_rejected() {
    let built = build("{ \"$app\": \"t.x@1.0.0\", \"$model\": {} }");
    assert!(built.has_code(code::MISSING_MEMBER));
}

#[test]
fn app_version_not_semver_rejected() {
    // Annex E.1: `1.0` lacks the patch component of a semantic version.
    let built = build("{ \"$liasse\": 1, \"$app\": \"t.app@1.0\", \"$model\": {} }");
    assert!(built.has_code(code::HEADER));
    assert!(built.points_at("1.0"));
}

#[test]
fn package_name_uppercase_rejected() {
    // §2.5: package-name components are lowercase.
    let built = build("{ \"$liasse\": 1, \"$app\": \"t.App@1.0.0\", \"$model\": {} }");
    assert!(built.has_code(code::HEADER));
}

#[test]
fn package_name_component_digit_start_rejected() {
    let built = build("{ \"$liasse\": 1, \"$app\": \"t.9x@1.0.0\", \"$model\": {} }");
    assert!(built.has_code(code::HEADER));
}

#[test]
fn unknown_semantics_choice_rejected() {
    let built = build(&app(
        "\"$semantics\": { \"made_up\": true }, \"$model\": { \"n\": \"text\" }",
    ));
    assert!(built.has_code(code::HEADER));
    assert!(built.points_at("made_up"));
    assert!(built.has_hint());
}

#[test]
fn unsupported_timestamp_precision_rejected() {
    let built = build(&app(
        "\"$semantics\": { \"timestamp_precision\": \"decades\" }, \"$model\": { \"n\": \"text\" }",
    ));
    assert!(built.has_code(code::HEADER));
}

#[test]
fn resource_descriptor_missing_member_rejected() {
    let built = build(&app(
        "\"$resources\": { \"tpl\": { \"$path\": \"resources/x.html\" } }, \"$model\": { \"n\": \"text\" }",
    ));
    assert!(built.has_code(code::MISSING_MEMBER));
}

#[test]
fn resource_path_escapes_archive_root_rejected() {
    let built = build(&app(
        "\"$resources\": { \"tpl\": { \"$path\": \"../secret\", \"$media\": \"text/html\", \"$sha256\": \"ab\" } }, \"$model\": { \"n\": \"text\" }",
    ));
    assert!(built.has_code(code::HEADER));
    assert!(built.has_hint());
}
