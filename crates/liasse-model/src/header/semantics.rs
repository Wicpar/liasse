//! The `$semantics` observable-choice object (SPEC.md §4.4, Annex A.5/A.6):
//! `timestamp_precision` and the `decimal_division` scale/rounding policy.

use liasse_syntax::DocValue;

use crate::doc::DocValueExt;
use crate::report::{code, Reporter};

pub(super) fn check_semantics(reporter: &mut Reporter, value: &DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(value.span, code::HEADER, "`$semantics` must be an object");
        return;
    };
    for member in members {
        match member.name.text.as_str() {
            "timestamp_precision" => check_timestamp_precision(reporter, &member.value),
            "decimal_division" => check_decimal_division(reporter, &member.value),
            other => reporter.reject_hint(
                member.span,
                code::HEADER,
                format!("`{other}` is not a standard `$semantics` choice"),
                "supported choices are `timestamp_precision` and `decimal_division`",
            ),
        }
    }
}

fn check_timestamp_precision(reporter: &mut Reporter, value: &DocValue) {
    let ok = value
        .as_string()
        .is_some_and(|text| liasse_value::Precision::parse(text).is_some());
    if !ok {
        reporter.reject_hint(
            value.span,
            code::HEADER,
            "`timestamp_precision` must be one of `s`, `ms`, `us`, `ns`",
            "e.g. `\"timestamp_precision\": \"us\"`",
        );
    }
}

/// Supported explicit rounding modes (A.6).
const ROUNDING_MODES: &[&str] = &[
    "half_even",
    "half_away_from_zero",
    "toward_zero",
    "away_from_zero",
    "floor",
    "ceiling",
];

fn check_decimal_division(reporter: &mut Reporter, value: &DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(
            value.span,
            code::HEADER,
            "`decimal_division` must be an object of `scale`/`rounding`",
        );
        return;
    };
    for member in members {
        match member.name.text.as_str() {
            "scale" => {}
            "rounding" => {
                let ok = member
                    .value
                    .as_string()
                    .is_some_and(|text| ROUNDING_MODES.contains(&text));
                if !ok {
                    reporter.reject_hint(
                        member.value.span,
                        code::HEADER,
                        "unsupported `decimal_division.rounding` mode",
                        "supported modes are listed in Annex A.6",
                    );
                }
            }
            other => reporter.reject(
                member.span,
                code::HEADER,
                format!("`{other}` is not a `decimal_division` setting"),
            ),
        }
    }
}
