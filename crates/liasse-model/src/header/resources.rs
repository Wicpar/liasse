//! The `$requires` namespace descriptors and `$resources` descriptor objects
//! (SPEC.md §4.1, §16.2). Shape-only in the CORE pass: resolving a namespace
//! against a host and verifying resource digests belong to a later phase.

use liasse_syntax::{DocMember, DocValue};

use crate::doc::DocValueExt;
use crate::report::{code, Reporter};

pub(super) fn check_requires(reporter: &mut Reporter, value: &DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(
            value.span,
            code::HEADER,
            "`$requires` maps namespace handles to `namespace@major` descriptors",
        );
        return;
    };
    for member in members {
        if member.value.as_string().is_none() {
            reporter.reject(
                member.value.span,
                code::HEADER,
                format!("`$requires.{}` must be a `namespace@major` string", member.name.text),
            );
        }
    }
}

pub(super) fn check_resources(reporter: &mut Reporter, value: &DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(value.span, code::HEADER, "`$resources` must be an object");
        return;
    };
    for member in members {
        check_one_resource(reporter, member);
    }
}

fn check_one_resource(reporter: &mut Reporter, member: &DocMember) {
    let Some(fields) = member.value.as_object() else {
        reporter.reject(
            member.value.span,
            code::HEADER,
            "a resource descriptor must be an object",
        );
        return;
    };
    for required in ["$path", "$media", "$sha256"] {
        if !fields.iter().any(|f| f.name.text == required) {
            reporter.reject_hint(
                member.value.span,
                code::MISSING_MEMBER,
                format!("resource `{}` is missing `{required}`", member.name.text),
                "a resource needs `$path`, `$media`, and `$sha256`",
            );
        }
    }
    for field in fields {
        match field.name.text.as_str() {
            "$path" => check_resource_path(reporter, field),
            "$media" | "$sha256" => {}
            other => reporter.reject(
                field.span,
                code::UNKNOWN_MEMBER,
                format!("`{other}` is not a resource-descriptor member"),
            ),
        }
    }
}

fn check_resource_path(reporter: &mut Reporter, field: &DocMember) {
    let Some(path) = field.value.as_string() else {
        reporter.reject(field.value.span, code::HEADER, "`$path` must be a string");
        return;
    };
    let escapes = path.starts_with('/')
        || path.split('/').any(|segment| segment == "..");
    if escapes {
        reporter.reject_hint(
            field.value.span,
            code::HEADER,
            format!("resource `$path` `{path}` must stay inside the archive root"),
            "use a relative path such as `resources/invoice.html`",
        );
    }
}
