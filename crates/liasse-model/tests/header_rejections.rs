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
fn requires_reserved_dollar_key_rejected() {
    // §2.5/§4.1: a `$requires` key is an application-defined namespace handle
    // and must be a valid declaration name; a `$`-reserved key (`$cbor`) is not.
    let built = build(&app(
        "\"$requires\": { \"$cbor\": \"liasse.cbor@1\" }, \"$model\": { \"n\": \"int = 0\" }",
    ));
    assert!(built.has_code(code::HEADER));
    assert!(built.points_at("$cbor"));
    assert!(built.has_hint());
}

#[test]
fn requires_unused_key_rejected() {
    // §16.2 (SPEC-ISSUES #17): every declared requirement MUST be used. A valid
    // handle that no expression calls pins a dead descriptor, so it is rejected.
    let built = build(&app(
        "\"$requires\": { \"cbor\": \"liasse.cbor@1\" }, \"$model\": { \"n\": \"int = 0\" }",
    ));
    assert!(built.has_code(code::HEADER));
    assert!(built.points_at("cbor"));
}

#[test]
fn requires_key_colliding_with_model_declaration_rejected() {
    // §16.2 (SPEC-ISSUES #17): a `$requires` key MUST be distinct from every
    // top-level model declaration name, so a bare identifier names one thing.
    let built = build(&app(
        "\"$requires\": { \"tasks\": \"test.util@1\" }, \"$model\": { \"tasks\": { \"$key\": \"id\", \"id\": \"text\" }, \"probe\": { \"$view\": \"tasks.f(1)\" } }",
    ));
    assert!(built.has_code(code::HEADER));
    assert!(built.points_at("tasks"));
}

#[test]
fn requires_key_shadowing_core_namespace_rejected() {
    // §16.1/§16.2: a `$requires` key equal to a core namespace name (`sha`) is
    // rejected — it must not rebind a trusted core namespace.
    let built = build(&app(
        "\"$requires\": { \"sha\": \"evil.sha@1\" }, \"$model\": { \"n\": \"int = 0\" }",
    ));
    assert!(built.has_code(code::HEADER));
    assert!(built.points_at("sha"));
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

/// §4.1: "an address supplied by both `$seed` (or `$data`) and `$bundle` is a
/// static load error". The two members carry distinct OWNERSHIP — `$seed` is
/// starting data the application's users own once instantiated, `$bundle` is
/// package-authoritative content the package maintains across releases (§13.13
/// applies apply-if-absent to one and a three-way merge to the other) — so an
/// address owned by both has two contradictory update rules and no way to pick.
///
/// A keyed-collection ROW is such an address: `notes.n1` here is supplied by
/// `$seed` and by `$bundle`, which the rejection must name.
#[test]
fn row_supplied_by_both_seed_and_bundle_rejected() {
    let built = build(&app(
        "\"$model\": { \"notes\": { \"$key\": \"id\", \"id\": \"text\", \"body\": \"text\" } }, \
         \"$seed\": { \"notes\": { \"n1\": { \"body\": \"user\" } } }, \
         \"$bundle\": { \"notes\": { \"n1\": { \"body\": \"package\" } } }",
    ));
    assert!(built.has_code(code::SEED), "the §4.1 overlap is a seed-class rejection: {}", built.rendered());
    assert!(
        built.rendered().contains("notes.n1"),
        "§4.1: the rejection names the shared ADDRESS, not just the member: {}",
        built.rendered(),
    );
    assert!(built.has_hint());
}

/// The same rule through the `$data` alias (§4.1: "`$data` is an alias of
/// `$seed`"), at a §8.2 root-singleton member — a whole-address overlap rather
/// than a shared row key.
#[test]
fn root_member_supplied_by_both_data_and_bundle_rejected() {
    let built = build(&app(
        "\"$model\": { \"motto\": \"text\" }, \"$data\": { \"motto\": \"user\" }, \
         \"$bundle\": { \"motto\": \"package\" }",
    ));
    assert!(built.has_code(code::SEED), "{}", built.rendered());
    assert!(built.rendered().contains("motto"), "{}", built.rendered());
    assert!(built.has_hint());
}

/// The control that keeps the two rejections above from passing for the wrong
/// reason: §4.1 forbids a SHARED address, not the coexistence of `$seed` and
/// `$bundle`. Disjoint rows of the same collection, and disjoint root members,
/// both load.
#[test]
fn disjoint_seed_and_bundle_addresses_accepted() {
    let built = build(&app(
        "\"$model\": { \"motto\": \"text\", \"theme\": \"text\", \
         \"notes\": { \"$key\": \"id\", \"id\": \"text\", \"body\": \"text\" } }, \
         \"$seed\": { \"motto\": \"user\", \"notes\": { \"n1\": { \"body\": \"user\" } } }, \
         \"$bundle\": { \"theme\": \"dark\", \"notes\": { \"n2\": { \"body\": \"package\" } } }",
    ));
    built.expect_ok();
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
