//! Blobs: accepted-type members and placement policy (SPEC.md §18, Annex C.17).
//!
//! Two static surfaces are validated here:
//!
//! * The accepted-blob-type members of an expanded field (§18.2): `$max_bytes`
//!   is an inclusive non-negative integer size limit and `$media` is the set of
//!   accepted media types. They are only meaningful on a `blob` field.
//! * The `$blob_storage` placement policy grammar (§18.4, C.17): `$in` is a
//!   required placement and `$serve` an optional store view, where a placement
//!   is a store-view leaf or one of `{ $all }`, `{ $any }`, `{ $copies, $of }`.
//!
//! CORE scope: a blob field's bytes are verified against the connector plan and
//! a store view is resolved against registered connectors at runtime; connector
//! capability checking is a documented host seam. Store-view leaves are checked
//! for shape (a non-empty expression string) but not fully typed here.

use liasse_syntax::DocValue;
use liasse_value::Type;

use crate::build::RawDecl;
use crate::doc::DocValueExt;
use crate::report::{code, Reporter};

/// Validate every collected `$blob_storage` placement policy.
pub(crate) fn check_all(reporter: &mut Reporter, blob_storage: &[RawDecl]) {
    for block in blob_storage {
        check_storage(reporter, block.value);
    }
}

/// §18.2: `$max_bytes` is a non-negative integer byte count (wire form is the
/// canonical decimal string of Annex A.1).
pub(crate) fn check_max_bytes(reporter: &mut Reporter, value: &DocValue) {
    let ok = match value.as_string() {
        Some(text) => text.parse::<u64>().is_ok(),
        None => value.as_number().is_some_and(|text| text.parse::<u64>().is_ok()),
    };
    if !ok {
        reporter.reject_hint(
            value.span,
            code::BLOB,
            "`$max_bytes` must be a non-negative integer byte count",
            "e.g. `\"$max_bytes\": \"10485760\"`",
        );
    }
}

/// §18.2: `$media` is a non-empty array of canonical media-type strings.
pub(crate) fn check_media(reporter: &mut Reporter, value: &DocValue) {
    let Some(items) = value.as_array() else {
        reporter.reject_hint(
            value.span,
            code::BLOB,
            "`$media` is an array of accepted media types",
            "e.g. `\"$media\": [\"application/pdf\", \"image/png\"]`",
        );
        return;
    };
    if items.is_empty() {
        reporter.reject(value.span, code::BLOB, "`$media` must accept at least one media type");
    }
    for item in items {
        let ok = item.as_string().is_some_and(|text| text.contains('/') && !text.trim().is_empty());
        if !ok {
            reporter.reject_hint(
                item.span,
                code::BLOB,
                "each `$media` entry is a `type/subtype` media type",
                "e.g. `\"application/pdf\"`",
            );
        }
    }
}

/// §18.2: reject `$max_bytes`/`$media` on a non-`blob` field.
pub(crate) fn require_blob_type(reporter: &mut Reporter, ty: &Type, span: liasse_diag::ByteSpan) {
    if !matches!(ty, Type::Blob) {
        reporter.reject_hint(
            span,
            code::BLOB,
            "`$max_bytes`/`$media` describe an accepted `blob` type",
            "set `\"$type\": \"blob\"` on this field",
        );
    }
}

/// Validate one `$blob_storage` placement policy object (§18.4).
pub(crate) fn check_storage(reporter: &mut Reporter, value: &DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(value.span, code::BLOB, "`$blob_storage` must be a placement object");
        return;
    };
    let mut has_in = false;
    for member in members {
        match member.name.text.as_str() {
            "$in" => {
                has_in = true;
                check_placement(reporter, &member.value);
            }
            "$serve" => check_store_view(reporter, &member.value),
            other => reporter.reject(
                member.span,
                code::BLOB,
                format!("`{other}` is not a `$blob_storage` member"),
            ),
        }
    }
    if !has_in {
        reporter.reject_hint(
            value.span,
            code::MISSING_MEMBER,
            "`$blob_storage` requires `$in`",
            "declare where copies are placed, e.g. `\"$in\": \"/stores['primary']\"`",
        );
    }
}

/// A placement is a store-view leaf or a `$all`/`$any`/`$copies` branch (C.17).
fn check_placement(reporter: &mut Reporter, value: &DocValue) {
    if value.as_string().is_some() {
        check_store_view(reporter, value);
        return;
    }
    let Some(members) = value.as_object() else {
        reporter.reject_hint(
            value.span,
            code::BLOB,
            "a placement is a store view or a `$all`/`$any`/`$copies` branch",
            "e.g. `{ \"$any\": [\"/stores['a']\", \"/stores['b']\"] }`",
        );
        return;
    };
    let names: Vec<&str> = members.iter().map(|m| m.name.text.as_str()).collect();
    match members {
        [only] if only.name.text == "$all" || only.name.text == "$any" => {
            check_branch_list(reporter, &only.value, only.name.text.as_str())
        }
        _ if names.contains(&"$copies") || names.contains(&"$of") => {
            check_copies(reporter, members)
        }
        _ => reporter.reject_hint(
            value.span,
            code::BLOB,
            "a placement branch is exactly one of `$all`, `$any`, or `$copies`+`$of`",
            "a store view is written as a string expression instead",
        ),
    }
}

fn check_branch_list(reporter: &mut Reporter, value: &DocValue, marker: &str) {
    let Some(items) = value.as_array() else {
        reporter.reject(
            value.span,
            code::BLOB,
            format!("`{marker}` is an array of placements"),
        );
        return;
    };
    if items.is_empty() {
        reporter.reject(value.span, code::BLOB, format!("`{marker}` must list at least one placement"));
    }
    for item in items {
        check_placement(reporter, item);
    }
}

fn check_copies(reporter: &mut Reporter, members: &[liasse_syntax::DocMember]) {
    let mut has_copies = false;
    let mut has_of = false;
    for member in members {
        match member.name.text.as_str() {
            "$copies" => {
                has_copies = true;
                check_copy_count(reporter, &member.value);
            }
            "$of" => {
                has_of = true;
                check_store_view(reporter, &member.value);
            }
            other => reporter.reject(
                member.span,
                code::BLOB,
                format!("`{other}` is not a `$copies` placement member"),
            ),
        }
    }
    if !(has_copies && has_of) {
        let span = members.first().map_or(liasse_diag::ByteSpan::point(0), |m| m.span);
        reporter.reject_hint(
            span,
            code::MISSING_MEMBER,
            "a `$copies` placement needs both `$copies` and `$of`",
            "e.g. `{ \"$copies\": 2, \"$of\": \"/stores[:s | s.enabled]\" }`",
        );
    }
}

fn check_copy_count(reporter: &mut Reporter, value: &DocValue) {
    let ok = match value.as_number() {
        Some(text) => text.parse::<u64>().is_ok_and(|n| n >= 1),
        None => value.as_string().is_some_and(|t| t.parse::<u64>().is_ok_and(|n| n >= 1)),
    };
    if !ok {
        reporter.reject(value.span, code::BLOB, "`$copies` must be a positive integer count");
    }
}

fn check_store_view(reporter: &mut Reporter, value: &DocValue) {
    let ok = value.as_string().is_some_and(|text| !text.trim().is_empty());
    if !ok {
        reporter.reject_hint(
            value.span,
            code::BLOB,
            "a store view is a non-empty view expression string",
            "e.g. `\"/stores['primary']\"`",
        );
    }
}
