//! History policy declaration (SPEC.md §19.3, Annex C.1).
//!
//! `$history` declares the minimum recoverable range for a package instance. Its
//! only two shapes are the literal `"all"` and the object `{ "$minimum":
//! duration }`; omitting it is equivalent to `all`. This validates that static
//! shape (the duration is parsed through [`liasse_value::Duration`]). Retention
//! enforcement, lineage materialization, and erasure are runtime concerns.

use liasse_syntax::DocValue;

use crate::doc::DocValueExt;
use crate::report::{code, Reporter};

/// Validate a top-level `$history` policy value.
pub(crate) fn check(reporter: &mut Reporter, value: &DocValue) {
    if value.as_string() == Some("all") {
        return;
    }
    if value.as_string().is_some() {
        reporter.reject_hint(
            value.span,
            code::HISTORY,
            "the string form of `$history` is only `all`",
            "use `\"$history\": \"all\"` or `{ \"$minimum\": \"P10Y\" }`",
        );
        return;
    }
    let Some(members) = value.as_object() else {
        reporter.reject_hint(
            value.span,
            code::HISTORY,
            "`$history` is `all` or an object with `$minimum`",
            "e.g. `{ \"$minimum\": \"P10Y\" }`",
        );
        return;
    };
    let mut has_minimum = false;
    for member in members {
        match member.name.text.as_str() {
            "$minimum" => {
                has_minimum = true;
                check_minimum(reporter, &member.value);
            }
            other => reporter.reject(
                member.span,
                code::HISTORY,
                format!("`{other}` is not a `$history` member"),
            ),
        }
    }
    if !has_minimum {
        reporter.reject_hint(
            value.span,
            code::MISSING_MEMBER,
            "an object `$history` requires `$minimum`",
            "set a retention floor, e.g. `\"$minimum\": \"P10Y\"`",
        );
    }
}

/// `$minimum` is a retention duration. §19.3's own example uses `P10Y`, whose
/// calendar year is outside the *fixed* `duration` type (A.4), so this checks
/// ISO-8601 duration *syntax* (a `P`-prefixed designator string with content)
/// rather than parsing it as a fixed duration.
fn check_minimum(reporter: &mut Reporter, value: &DocValue) {
    let ok = value.as_string().is_some_and(|text| {
        let text = text.trim();
        text.len() > 1 && text.starts_with('P') && text[1..].chars().any(|c| c.is_ascii_digit())
    });
    if !ok {
        reporter.reject_hint(
            value.span,
            code::HISTORY,
            "`$minimum` must be an ISO-8601 duration",
            "use a duration such as `P10Y` or `P30D`",
        );
    }
}
