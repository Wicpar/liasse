//! Migration declarations (SPEC.md §20, Annex C.15).
//!
//! `$migrations` maps an exact source package version to one ordered atomic
//! migration program — an array of mutation statements over the prospective
//! target state (§20.1). This validates that static shape: each key parses as a
//! `major.minor.patch` version, and each program is a non-empty array of
//! statement strings that parse. Field-level `$from`/`$as`/`$back` mapping
//! members are accepted structurally by the field builder.
//!
//! CORE scope: migration statements read `$old` (source state) and `.` (the
//! prospective target); typing them requires both the source and target row
//! models and the reversible round-trip check (`$back($as(x)) == x`, §20.2).
//! Those need the two-model migration runtime and are documented seams — this
//! pass parses statements for syntax only.

use liasse_diag::SourceMap;
use liasse_syntax::parse_expression;

use crate::doc::DocValueExt;
use crate::names::Version;
use crate::report::{code, Reporter};

/// Validate a top-level `$migrations` object.
pub(crate) fn check(reporter: &mut Reporter, sources: &mut SourceMap, value: &liasse_syntax::DocValue) {
    let Some(entries) = value.as_object() else {
        reporter.reject_hint(
            value.span,
            code::MIGRATION,
            "`$migrations` maps a source package version to a migration program",
            "e.g. `\"1.4.0\": [\".people = $old.users { id }\"]`",
        );
        return;
    };
    for entry in entries {
        if let Err(reason) = Version::parse(&entry.name.text) {
            reporter.reject_hint(
                entry.name.span,
                code::MIGRATION,
                format!("`{}` is not an exact source package version: {reason}", entry.name.text),
                "the key is the exact `major.minor.patch` of the source package",
            );
        }
        check_program(reporter, sources, &entry.value);
    }
}

fn check_program(reporter: &mut Reporter, sources: &mut SourceMap, value: &liasse_syntax::DocValue) {
    let Some(statements) = value.as_array() else {
        reporter.reject_hint(
            value.span,
            code::MIGRATION,
            "a migration program is an array of mutation statements",
            "wrap the statements in an array, even a single one",
        );
        return;
    };
    if statements.is_empty() {
        reporter.reject(value.span, code::MIGRATION, "a migration program must have at least one statement");
    }
    for statement in statements {
        let Some(text) = statement.as_string() else {
            reporter.reject(statement.span, code::MIGRATION, "each migration statement is an expression string");
            continue;
        };
        let sub = sources.add_label("migration", text.to_owned());
        if let Err(diags) = parse_expression(sub, text) {
            reporter.emit_all(diags);
        }
    }
}
