//! Managed keyrings (SPEC.md §17, Annex C.16).
//!
//! A `$keyring` declaration is validated for its observable *policy shape*: the
//! required `$provider`/`$algorithm`, the optional `$usage`/`$rotate`/
//! `$retain`/`$protection`, and the `$rotate` object's `$every`/`$overlap`/
//! `$mode`. Durations are parsed through [`liasse_value::Duration`] so a
//! malformed cadence is caught at load. A `$keyring` object accepts no
//! application-defined members, so any member outside C.16 is rejected (§2.5).
//!
//! CORE scope: matching the declaration against the registered provider's
//! advertised capabilities (algorithm/key-type, operation set, generation vs
//! binding mode, protection class, destroy/attest behaviour — §17.6) needs the
//! host key-provider descriptor and is a documented runtime seam. Keyring-
//! managed version metadata is likewise provider-owned and never `$data`-seeded
//! (§9.1); that seed rejection belongs to the seed phase's host-aware pass.

use liasse_syntax::DocValue;
use liasse_value::Duration;

use crate::doc::DocValueExt;
use crate::report::{code, Reporter};

/// Validate one `$keyring` policy object.
pub(crate) fn check(reporter: &mut Reporter, value: &DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(value.span, code::KEYRING, "`$keyring` must be a policy object");
        return;
    };
    let mut has_provider = false;
    let mut has_algorithm = false;
    for member in members {
        match member.name.text.as_str() {
            "$provider" => {
                has_provider = true;
                check_name(reporter, &member.value, "$provider");
            }
            "$algorithm" => {
                has_algorithm = true;
                check_name(reporter, &member.value, "$algorithm");
            }
            "$usage" => check_usage(reporter, &member.value),
            "$rotate" => check_rotate(reporter, &member.value),
            "$retain" => check_duration(reporter, &member.value, "$retain"),
            "$protection" => check_name(reporter, &member.value, "$protection"),
            other => reporter.reject_hint(
                member.span,
                code::KEYRING,
                format!("`{other}` is not a `$keyring` member"),
                "keyring policy members are `$provider`, `$algorithm`, `$usage`, `$rotate`, `$retain`, `$protection` (C.16)",
            ),
        }
    }
    if !has_provider {
        reporter.reject_hint(
            value.span,
            code::MISSING_MEMBER,
            "a `$keyring` requires a `$provider`",
            "name the registered key provider, e.g. `\"$provider\": \"session-hsm\"`",
        );
    }
    if !has_algorithm {
        reporter.reject_hint(
            value.span,
            code::MISSING_MEMBER,
            "a `$keyring` requires an `$algorithm`",
            "select the key algorithm, e.g. `\"$algorithm\": \"Ed25519\"`",
        );
    }
}

fn check_name(reporter: &mut Reporter, value: &DocValue, member: &str) {
    let ok = value.as_string().is_some_and(|text| !text.trim().is_empty());
    if !ok {
        reporter.reject(
            value.span,
            code::KEYRING,
            format!("`{member}` must be a non-empty name"),
        );
    }
}

fn check_usage(reporter: &mut Reporter, value: &DocValue) {
    let Some(items) = value.as_array() else {
        reporter.reject_hint(
            value.span,
            code::KEYRING,
            "`$usage` is an array of permitted key operations",
            "e.g. `\"$usage\": [\"sign\"]`",
        );
        return;
    };
    for item in items {
        check_name(reporter, item, "$usage entry");
    }
}

/// `$rotate` is a shorthand duration cadence or an object of
/// `$every`/`$overlap?`/`$mode?` (§17.1).
fn check_rotate(reporter: &mut Reporter, value: &DocValue) {
    if value.as_string().is_some() {
        check_duration(reporter, value, "$rotate");
        return;
    }
    let Some(members) = value.as_object() else {
        reporter.reject_hint(
            value.span,
            code::KEYRING,
            "`$rotate` is a duration or an object of `$every`/`$overlap`/`$mode`",
            "e.g. `\"$rotate\": \"P30D\"`",
        );
        return;
    };
    let mut has_every = false;
    for member in members {
        match member.name.text.as_str() {
            "$every" => {
                has_every = true;
                check_duration(reporter, &member.value, "$every");
            }
            "$overlap" => check_duration(reporter, &member.value, "$overlap"),
            "$mode" => check_mode(reporter, &member.value),
            other => reporter.reject(
                member.span,
                code::KEYRING,
                format!("`{other}` is not a `$rotate` member"),
            ),
        }
    }
    if !has_every {
        reporter.reject_hint(
            value.span,
            code::MISSING_MEMBER,
            "an object `$rotate` requires `$every`",
            "set the cadence, e.g. `\"$every\": \"P30D\"`",
        );
    }
}

fn check_mode(reporter: &mut Reporter, value: &DocValue) {
    let ok = matches!(value.as_string(), Some("automatic" | "manual"));
    if !ok {
        reporter.reject_hint(
            value.span,
            code::KEYRING,
            "`$mode` is `automatic` or `manual`",
            "omit `$mode` to default to `automatic`",
        );
    }
}

fn check_duration(reporter: &mut Reporter, value: &DocValue, member: &str) {
    let Some(text) = value.as_string() else {
        reporter.reject(
            value.span,
            code::KEYRING,
            format!("`{member}` must be an ISO-8601 duration string"),
        );
        return;
    };
    if Duration::parse(text).is_err() {
        reporter.reject_hint(
            value.span,
            code::KEYRING,
            format!("`{member}` is not a valid ISO-8601 duration"),
            "use a duration such as `P30D`",
        );
    }
}
