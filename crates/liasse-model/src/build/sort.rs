//! `$sort` declaration validation (SPEC.md §7.3).
//!
//! A collection or view MAY declare `$sort`: an array of successive comparison
//! keys from highest to lowest priority. Each key is either a string
//! (`"name"`, or `"-name"` for descending), or the structured object form
//! `{ "$by": "name", "$dir": "asc" | "desc" }`. This validates that form; the
//! keys are resolved against the row shape by the runtime, a documented seam
//! consistent with the meter `$order` handling.

use liasse_syntax::DocValue;

use crate::doc::DocValueExt;
use crate::report::{code, Reporter};

/// Validate a `$sort` declaration's structure (§7.3).
pub(super) fn check(reporter: &mut Reporter, value: &DocValue) {
    let Some(keys) = value.as_array() else {
        reporter.reject_hint(
            value.span,
            code::SORT,
            "`$sort` is an array of comparison keys",
            "e.g. `\"$sort\": [\"name\", \"-created_at\"]`",
        );
        return;
    };
    if keys.is_empty() {
        reporter.reject(value.span, code::SORT, "`$sort` must list at least one comparison key");
        return;
    }
    for key in keys {
        check_key(reporter, key);
    }
}

/// Validate one `$sort` key: a non-empty string (optionally `-`-prefixed) or a
/// `{ $by, $dir }` object.
fn check_key(reporter: &mut Reporter, key: &DocValue) {
    if let Some(text) = key.as_string() {
        // A leading `-` reverses the key; the remainder names the field.
        if text.strip_prefix('-').unwrap_or(text).trim().is_empty() {
            reporter.reject(key.span, code::SORT, "a `$sort` key must name a field");
        }
        return;
    }
    let Some(members) = key.as_object() else {
        reporter.reject_hint(
            key.span,
            code::SORT,
            "a `$sort` key is a field name string or a `{ $by, $dir }` object",
            "e.g. `\"-created_at\"` or `{ \"$by\": \"name\", \"$dir\": \"asc\" }`",
        );
        return;
    };
    let mut has_by = false;
    for member in members {
        match member.name.text.as_str() {
            "$by" => {
                has_by = true;
                if member.value.as_string().is_none_or(str::is_empty) {
                    reporter.reject(member.value.span, code::SORT, "`$by` must name a field");
                }
            }
            "$dir" => match member.value.as_string() {
                Some("asc" | "desc") => {}
                _ => reporter.reject_hint(
                    member.value.span,
                    code::SORT,
                    "`$dir` must be `\"asc\"` or `\"desc\"`",
                    "omit `$dir` for ascending, or set `\"desc\"` for descending",
                ),
            },
            other => reporter.reject(
                member.span,
                code::SORT,
                format!("`{other}` is not a `$sort` key member; use `$by` and `$dir`"),
            ),
        }
    }
    if !has_by {
        reporter.reject_hint(
            key.span,
            code::SORT,
            "a structured `$sort` key requires `$by`",
            "name the field with `\"$by\": \"field\"`",
        );
    }
}
