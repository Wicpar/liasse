//! A map declaration IS a collection declaration (SPEC §5.4, §10.1).
//!
//! §5.4 makes the map the *degenerate keyed collection*: "a map IS a table, so
//! every access a keyed collection has applies to it unchanged". Nothing in §5.4
//! or §10 says a map declaration is a weaker position than a table's, so the
//! declarations a collection carries — `$mut` row mutations (§8.2), the `$roles`
//! blocks nested on it (§10.3), `$check` (§5.10), `$unique` (§5.7), `$sort` —
//! must be collected off a map exactly as off a table.
//!
//! Before this was true they were dropped on the floor: the map form read only
//! `$key`/`$value` and never walked the rest of the declaration, so a row
//! mutation declared on a map reached neither [`Model::mutations`] nor the
//! surface phase, and a `$public`/`$roles` surface over it resolved NOTHING —
//! every call answered `Denied(Unresolved)` while an identical table passed.
//!
//! Each test states the map form and the table form of the SAME declaration and
//! asserts they agree, so nothing here is tautological: the table spelling is the
//! independent oracle.

mod common;

use common::build;
use liasse_model::Model;

/// A package whose `$model` is `members`.
fn package(members: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "t.mapdecl@1.0.0",
          "$model": {{ {members} }}
        }}"#
    )
}

/// `settings` as a MAP carrying `extra` beside its two markers.
fn map_form(extra: &str) -> String {
    format!(r#""settings": {{ "$key": "text", "$value": "text" {extra} }}"#)
}

/// `settings` as an ordinary keyed collection, `$value` written out as `value`.
fn table_form(extra: &str) -> String {
    format!(r#""settings": {{ "$key": "name", "name": "text", "value": "text" {extra} }}"#)
}

/// Each declared mutation as `(name, receiver path)`.
fn mutations(model: &Model) -> Vec<(String, Vec<String>)> {
    model.mutations().iter().map(|m| (m.name.as_str().to_owned(), m.path.clone())).collect()
}

/// Each exposed surface as `(name, public, calls)`.
fn surfaces(model: &Model) -> Vec<(String, bool, Vec<String>)> {
    model
        .surfaces()
        .iter()
        .map(|s| {
            let calls = s.calls.iter().map(|c| c.as_str().to_owned()).collect();
            (s.name.as_str().to_owned(), s.public, calls)
        })
        .collect()
}

/// The surface exposing `settings`'s row mutation, identical for both spellings.
const SURFACE: &str =
    r#", "$public": { "config": { "$mut": { "retitle": ".settings[@name].retitle" } } }"#;

#[test]
fn a_row_mutation_declared_on_a_map_is_a_declared_mutation() {
    // §8.2: a `$mut` inside a collection declares a ROW mutation whose receiver is
    // that collection's row. A map entry is an ordinary row, so the map form
    // registers the same mutation at the same receiver path the table form does.
    let map = build(&package(&map_form(r#", "$mut": { "retitle": ".$value = @v" }"#)));
    let table = build(&package(&table_form(r#", "$mut": { "retitle": ".value = @v" }"#)));
    let expected = vec![("retitle".to_owned(), vec!["settings".to_owned()])];
    assert_eq!(mutations(table.expect_ok()), expected, "the table spelling is the oracle");
    assert_eq!(
        mutations(map.expect_ok()),
        expected,
        "a `$mut` on a MAP declares the same row mutation the table spelling does (§5.4)"
    );
}

#[test]
fn a_surface_over_a_map_exposes_its_row_mutation() {
    // §10.1: a surface `$mut` names a declared mutation through a receiver that
    // selects exactly one row. Over a map that receiver is one ENTRY. This is the
    // exposure path that resolved nothing while the map's `$mut` was dropped.
    let map = build(&package(&format!(
        "{}{SURFACE}",
        map_form(r#", "$mut": { "retitle": ".$value = @v" }"#)
    )));
    let table = build(&package(&format!(
        "{}{SURFACE}",
        table_form(r#", "$mut": { "retitle": ".value = @v" }"#)
    )));
    let expected = vec![("config".to_owned(), true, vec!["retitle".to_owned()])];
    assert_eq!(surfaces(table.expect_ok()), expected, "the table spelling is the oracle");
    assert_eq!(
        surfaces(map.expect_ok()),
        expected,
        "a `$public` surface exposes a MAP's row mutation exactly as a table's (§10.1)"
    );
}

#[test]
fn a_roles_block_nested_on_a_map_is_collected() {
    // §10.3: "Roles MAY be nested on application rows. Their location defines
    // scope." A map entry is an application row, so a `$roles` block declared on a
    // map reaches the surface phase — it used to be dropped, leaving the role
    // nonexistent and every call to it `Denied(Unresolved)`.
    let model = build(&package(
        r#""accounts": { "$key": "id", "id": "text" },
           "settings": { "$key": "text", "$value": "text",
             "$mut": { "retitle": ".$value = @v" },
             "$roles": { "owner": { "$auth": "token", "$members": "/accounts",
               "entry": { "$mut": { "retitle": ".retitle" } } } } },
           "$auth": { "token": { "$credential": "text", "$verify": "$credential",
             "$actor": "/accounts[$proof]" } }"#,
    ));
    assert_eq!(
        surfaces(model.expect_ok()),
        vec![("entry".to_owned(), false, vec!["retitle".to_owned()])],
        "a `$roles` block nested on a MAP declares its surfaces (§10.3)"
    );
}

#[test]
fn a_check_declared_on_a_map_is_collected() {
    // §5.10: a collection-level `$check` constrains its rows. A map's rows are
    // ordinary rows, so the check is collected — and, being collected, must TYPE:
    // a `$check` reading a member the row does not have is rejected rather than
    // quietly discarded with the rest of the declaration.
    build(&package(&map_form(r#", "$check": ".$value != \"\"""#))).expect_ok();
    let bad = build(&package(&map_form(r#", "$check": ".nope != \"\"""#)));
    assert!(
        bad.expect_err().iter().count() > 0,
        "a `$check` on a map is type-checked against the map row, not ignored"
    );
}

#[test]
fn an_unknown_reserved_member_on_a_map_is_rejected() {
    // A map declaration is a shape, so the reserved-member dispatch judges it: an
    // undefined `$` member is the same named error a table's is, never a silent
    // omission.
    let built = build(&package(&map_form(r#", "$nonsense": true"#)));
    assert!(
        built.has_code("M-RESERVED"),
        "an unknown `$` member on a map is rejected as it is on a table; codes: {:?}",
        built.codes()
    );
}

#[test]
fn an_authored_field_on_a_map_is_rejected() {
    // §5.4: a map's row shape is the FIXED `{ $key, $value }`. An authored field
    // has nowhere to go, so it is a named static error — the declaration is not
    // silently accepted with the field missing from the row.
    let built = build(&package(&map_form(r#", "label": "text""#)));
    assert!(built.has_code("M-SHAPE"), "codes: {:?}", built.codes());
    assert!(
        built.rendered().contains("not a member of a map"),
        "the diagnostic names the map row shape: {}",
        built.rendered()
    );
}

#[test]
fn a_unique_candidate_key_over_a_map_value_resolves() {
    // §5.7: a collection MAY declare candidate keys. A map's row members are named
    // `$key`/`$value` (§5.4) — names no authored field can spell — so `$unique`
    // naming `$value` is the map spelling of "each value occurs at most once".
    build(&package(&map_form(r#", "$unique": [["$value"]]"#))).expect_ok();
    let bad = build(&package(&map_form(r#", "$unique": [["nope"]]"#)));
    assert!(
        bad.expect_err().iter().count() > 0,
        "a `$unique` naming a member the map row does not have is still rejected"
    );
}
