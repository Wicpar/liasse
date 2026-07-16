//! Authenticator declarations (SPEC.md §11, Annex C.12).
//!
//! A scope's one `$auth` object maps names to authenticators or explicit parent
//! aliases. This validates their static shape:
//!
//! * an authenticator object carries a required `$credential` type, a required
//!   `$verify` expression, a required `$actor` exact-one-row expression, an
//!   optional `$session` expression, and an optional `$check` (one expression or
//!   a sequence); any other member is rejected (§2.5, C.12);
//! * a `"$parent.<name>"` string is a parent-alias member (§11.9, §13.11);
//! * every role's `$auth` selection names a declared authenticator (§11.4).
//!
//! CORE scope: authenticator expressions are *parsed* (syntax is checked) but
//! not fully typed. `$verify` binds `$proof` through a host verifier namespace
//! whose typed signature the model does not hold, and `$session`/`$actor`
//! resolve rows the namespace proof selects; typing them needs the §16 host
//! descriptor and is a documented seam. Alias resolution across module scopes is
//! likewise a runtime seam — names are collected across every `$auth` block.

use std::collections::BTreeSet;

use liasse_diag::SourceMap;
use liasse_syntax::{parse_expression, DocValue};

use crate::build::{RawDecl, RawSurface};
use crate::doc::DocValueExt;
use crate::report::{code, Reporter};
use crate::types::{NamedTypes, TypeParser};

/// Validate every `$auth` block and each role's authenticator selection.
pub(crate) fn check(
    reporter: &mut Reporter,
    sources: &mut SourceMap,
    auths: &[RawDecl],
    surfaces: &[RawSurface],
) {
    let mut names = BTreeSet::new();
    for block in auths {
        check_block(reporter, sources, block, &mut names);
    }
    for block in surfaces {
        if !block.public {
            check_role_selections(reporter, block, &names);
        }
    }
}

fn check_block(
    reporter: &mut Reporter,
    sources: &mut SourceMap,
    block: &RawDecl,
    names: &mut BTreeSet<String>,
) {
    let Some(members) = block.value.as_object() else {
        reporter.reject(block.span, code::AUTH, "`$auth` must be an object of authenticators");
        return;
    };
    for member in members {
        names.insert(member.name.text.clone());
        if let Some(alias) = member.value.as_string() {
            if !alias.trim_start().starts_with("$parent") {
                reporter.reject_hint(
                    member.value.span,
                    code::AUTH,
                    "a string `$auth` member is a `$parent.<name>` alias",
                    "declare an authenticator object, or alias a parent authenticator with `$parent.<name>`",
                );
            }
            continue;
        }
        check_authenticator(reporter, sources, member);
    }
}

fn check_authenticator(reporter: &mut Reporter, sources: &mut SourceMap, member: &liasse_syntax::DocMember) {
    let Some(fields) = member.value.as_object() else {
        reporter.reject(
            member.value.span,
            code::AUTH,
            format!("authenticator `{}` must be an object", member.name.text),
        );
        return;
    };
    let mut has_credential = false;
    let mut has_verify = false;
    let mut has_actor = false;
    for field in fields {
        match field.name.text.as_str() {
            "$credential" => {
                has_credential = true;
                check_credential_type(reporter, &field.value);
            }
            "$verify" => {
                has_verify = true;
                parse_only(reporter, sources, &field.value, "$verify");
            }
            "$session" => parse_only(reporter, sources, &field.value, "$session"),
            "$actor" => {
                has_actor = true;
                parse_only(reporter, sources, &field.value, "$actor");
            }
            "$check" => check_conditions(reporter, sources, &field.value),
            other => reporter.reject(
                field.span,
                code::AUTH,
                format!("`{other}` is not an authenticator member"),
            ),
        }
    }
    require(reporter, member.value.span, has_credential, "$credential");
    require(reporter, member.value.span, has_verify, "$verify");
    require(reporter, member.value.span, has_actor, "$actor");
}

fn require(reporter: &mut Reporter, span: liasse_diag::ByteSpan, present: bool, member: &str) {
    if !present {
        reporter.reject_hint(
            span,
            code::MISSING_MEMBER,
            format!("an authenticator requires `{member}`"),
            "declare `$credential`, `$verify`, and `$actor` (C.12)",
        );
    }
}

fn check_credential_type(reporter: &mut Reporter, value: &DocValue) {
    let Some(text) = value.as_string() else {
        reporter.reject(value.span, code::AUTH, "`$credential` must be a type string");
        return;
    };
    if let Err(reason) = TypeParser::parse(text.trim(), &NamedTypes::new()) {
        reporter.reject(value.span, code::AUTH, reason);
    }
}

/// A `$check` is one boolean condition or a sequence of them (§11.3).
fn check_conditions(reporter: &mut Reporter, sources: &mut SourceMap, value: &DocValue) {
    if value.as_string().is_some() {
        parse_only(reporter, sources, value, "$check");
        return;
    }
    let Some(items) = value.as_array() else {
        reporter.reject(value.span, code::AUTH, "`$check` is one condition or a sequence of conditions");
        return;
    };
    for item in items {
        parse_only(reporter, sources, item, "$check");
    }
}

/// Parse (syntax-check) an authenticator expression without full typing.
fn parse_only(reporter: &mut Reporter, sources: &mut SourceMap, value: &DocValue, member: &str) {
    let Some(text) = value.as_string() else {
        reporter.reject(value.span, code::AUTH, format!("`{member}` must be an expression string"));
        return;
    };
    if text.trim().is_empty() {
        reporter.reject(value.span, code::AUTH, format!("`{member}` must not be empty"));
        return;
    }
    let sub = sources.add_label("auth", text.to_owned());
    if let Err(diags) = parse_expression(sub, text) {
        reporter.emit_all(diags);
    }
}

/// §11.4: each role's `$auth` names one or more declared authenticators.
fn check_role_selections(reporter: &mut Reporter, block: &RawSurface, names: &BTreeSet<String>) {
    let Some(roles) = block.value.as_object() else {
        return;
    };
    for role in roles {
        let Some(members) = role.value.as_object() else {
            continue;
        };
        for member in members {
            if member.name.text == "$auth" {
                check_selection(reporter, &member.value, names, &role.name.text);
            }
        }
    }
}

fn check_selection(reporter: &mut Reporter, value: &DocValue, names: &BTreeSet<String>, role: &str) {
    let selected: Vec<&str> = if let Some(one) = value.as_string() {
        vec![one]
    } else if let Some(items) = value.as_array() {
        items.iter().filter_map(|v| v.as_string()).collect()
    } else {
        reporter.reject(value.span, code::AUTH, "`$auth` is one authenticator name or an array of names");
        return;
    };
    for name in selected {
        if !names.contains(name) {
            reporter.reject_hint(
                value.span,
                code::AUTH,
                format!("role `{role}` selects authenticator `{name}`, which is not declared in `$auth`"),
                "declare the authenticator in a `$auth` block or alias it from the parent",
            );
        }
    }
}

