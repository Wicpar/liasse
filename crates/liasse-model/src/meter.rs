//! Meters: capacity pools and spends (SPEC.md §15, Annex C.14).
//!
//! Two declarations are validated statically:
//!
//! * `$limits` maps meter names to `{ $sources, $eligible?, $order? }`, where
//!   `$sources` maps a stable label to a pool view; unknown members are rejected
//!   (§2.5, C.14). Pool views, `$eligible`, and `$order` expressions are parsed
//!   for syntax.
//! * `$consumes` names one meter (scalar) or maps meters to amount expressions /
//!   consume configs. Every named meter MUST resolve to a `$limits` declaration
//!   on the spending collection's ancestor chain within the package instance
//!   (§15.1, §15.4); an unresolved meter name is rejected.
//!
//! CORE scope: meter expressions are *parsed* but not fully typed. A pool view
//! assigns the `$quantity` structural projection role and `$eligible` reads
//! `pool`/`spend` bindings whose row shapes come from the (untyped) pool views;
//! the standard expression checker has no `$quantity` projection directive, so
//! full pool typing, `$eligible` typing, and the §15.6 parameterless-accessor
//! rule are documented runtime seams. Reachability is computed over lexical
//! ancestors only; cross-module meter access (§15.5) is a separate seam.

use std::collections::{BTreeMap, BTreeSet};

use liasse_diag::SourceMap;
use liasse_syntax::parse_expression;

use crate::build::RawDecl;
use crate::doc::DocValueExt;
use crate::report::{code, Reporter};

/// Validate every `$limits` and `$consumes` declaration.
pub(crate) fn check(
    reporter: &mut Reporter,
    sources: &mut SourceMap,
    limits: &[RawDecl],
    consumes: &[RawDecl],
) {
    let mut declared: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();
    for block in limits {
        let names = check_limits(reporter, sources, block);
        declared.entry(block.path.clone()).or_default().extend(names);
    }
    for block in consumes {
        check_consumes(reporter, sources, block, &declared);
    }
}

/// Validate one `$limits` object, returning the meter names it declares.
fn check_limits(reporter: &mut Reporter, sources: &mut SourceMap, block: &RawDecl) -> Vec<String> {
    let Some(meters) = block.value.as_object() else {
        reporter.reject(block.span, code::METER, "`$limits` maps meter names to meter declarations");
        return Vec::new();
    };
    let mut names = Vec::new();
    for meter in meters {
        names.push(meter.name.text.clone());
        check_meter(reporter, sources, meter);
    }
    names
}

fn check_meter(reporter: &mut Reporter, sources: &mut SourceMap, meter: &liasse_syntax::DocMember) {
    let Some(members) = meter.value.as_object() else {
        reporter.reject(
            meter.value.span,
            code::METER,
            format!("meter `{}` must be an object", meter.name.text),
        );
        return;
    };
    let mut has_sources = false;
    for member in members {
        match member.name.text.as_str() {
            "$sources" => {
                has_sources = true;
                check_sources(reporter, sources, &member.value);
            }
            "$eligible" => parse_only(reporter, sources, &member.value, "$eligible"),
            "$order" => check_order(reporter, sources, &member.value),
            other => reporter.reject(
                member.span,
                code::METER,
                format!("`{other}` is not a meter member"),
            ),
        }
    }
    if !has_sources {
        reporter.reject_hint(
            meter.value.span,
            code::MISSING_MEMBER,
            format!("meter `{}` requires `$sources`", meter.name.text),
            "map at least one source label to a pool view",
        );
    }
}

fn check_sources(reporter: &mut Reporter, sources: &mut SourceMap, value: &liasse_syntax::DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(value.span, code::METER, "`$sources` maps source labels to pool views");
        return;
    };
    if members.is_empty() {
        reporter.reject(value.span, code::METER, "`$sources` must declare at least one pool view");
    }
    for member in members {
        parse_only(reporter, sources, &member.value, "a pool view");
    }
}

fn check_order(reporter: &mut Reporter, sources: &mut SourceMap, value: &liasse_syntax::DocValue) {
    let Some(items) = value.as_array() else {
        reporter.reject_hint(
            value.span,
            code::METER,
            "`$order` is an array of sort expressions",
            "e.g. `\"$order\": [\"$until\", \"price\"]`",
        );
        return;
    };
    for item in items {
        parse_only(reporter, sources, item, "an `$order` key");
    }
}

/// Validate `$consumes` and that every named meter resolves to an ancestor
/// `$limits` (§15.1, §15.4).
fn check_consumes(
    reporter: &mut Reporter,
    sources: &mut SourceMap,
    block: &RawDecl,
    declared: &BTreeMap<Vec<String>, BTreeSet<String>>,
) {
    let meters = consumed_meters(reporter, sources, block);
    for name in meters {
        if !reachable(&block.path, &name, declared) {
            reporter.reject_hint(
                block.span,
                code::METER,
                format!("`$consumes` names meter `{name}`, which no ancestor `$limits` declares"),
                "declare the meter with `$limits` on this row or an ancestor row in the same package",
            );
        }
    }
}

/// The meter names a `$consumes` value references.
fn consumed_meters(reporter: &mut Reporter, sources: &mut SourceMap, block: &RawDecl) -> Vec<String> {
    if let Some(name) = block.value.as_string() {
        return vec![name.trim().to_owned()];
    }
    let Some(members) = block.value.as_object() else {
        reporter.reject_hint(
            block.span,
            code::METER,
            "`$consumes` is a meter name or a map of meters to amounts/configs",
            "e.g. `\"$consumes\": \"credits\"`",
        );
        return Vec::new();
    };
    for member in members {
        // A config object overrides `$amount`/`$time` and carries metadata; a
        // scalar value is the amount expression. Both are parsed for syntax.
        if let Some(config) = member.value.as_object() {
            for field in config {
                parse_only(reporter, sources, &field.value, "a consume expression");
            }
        } else {
            parse_only(reporter, sources, &member.value, "a consume amount");
        }
    }
    members.iter().map(|m| m.name.text.clone()).collect()
}

/// Whether `name` is declared by a `$limits` at `path` or any ancestor prefix.
fn reachable(path: &[String], name: &str, declared: &BTreeMap<Vec<String>, BTreeSet<String>>) -> bool {
    declared.iter().any(|(decl_path, names)| {
        path.starts_with(decl_path.as_slice()) && names.contains(name)
    })
}

fn parse_only(reporter: &mut Reporter, sources: &mut SourceMap, value: &liasse_syntax::DocValue, what: &str) {
    let Some(text) = value.as_string() else {
        reporter.reject(value.span, code::METER, format!("{what} must be an expression string"));
        return;
    };
    if text.trim().is_empty() {
        reporter.reject(value.span, code::METER, format!("{what} must not be empty"));
        return;
    }
    let sub = sources.add_label("meter", text.to_owned());
    if let Err(diags) = parse_expression(sub, text) {
        reporter.emit_all(diags);
    }
}
