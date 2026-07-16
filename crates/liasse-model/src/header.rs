//! The package header (SPEC.md §4, Annex C.1, Annex E).
//!
//! Validates the top-level definition object: the required `$liasse` language
//! generation (checked first, §4.1), the exclusive `$app`/`$module` identity,
//! and the shape of `$semantics`, `$requires`, and `$resources`. The Annex C.1
//! grammar is closed, so any member outside it — including a reserved
//! `$`-member that is not a defined declaration — is rejected (§2.5).
//!
//! CORE scope: `$requires` is validated for *shape* only; resolving a namespace
//! against a host and verifying resource digests belong to a later pass, so
//! those are not performed here.

use liasse_syntax::{DocMember, DocValue};

use crate::doc::DocValueExt;
use crate::names::PackageId;
use crate::report::{code, Reporter};

/// The supported `$liasse` language generation (this specification, §4.1).
const SUPPORTED_GENERATION: i64 = 1;

/// Which identity marker a package declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A root application (`$app`).
    Application,
    /// A reusable module (`$module`).
    Module,
}

/// A validated package header.
#[derive(Debug, Clone)]
pub struct Header {
    /// Whether this is an application or a module definition.
    pub kind: Kind,
    /// The `name@version` identity.
    pub identity: PackageId,
}

/// The header plus the located sub-objects the rest of the build consumes.
pub(crate) struct Parsed<'a> {
    pub header: Header,
    pub model: Option<&'a DocValue>,
    pub types: Option<&'a DocValue>,
    pub data: Option<&'a DocValue>,
}

/// The members Annex C.1 permits at the top level, by package kind.
fn allowed_members(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Application => &[
            "$liasse",
            "$app",
            "$semantics",
            "$requires",
            "$resources",
            "$types",
            "$model",
            "$data",
            "$history",
            "$migrations",
        ],
        Kind::Module => &[
            "$liasse",
            "$module",
            "$semantics",
            "$requires",
            "$resources",
            "$types",
            "$config",
            "$model",
            "$data",
            "$history",
            "$use",
            "$deps",
            "$expose",
            "$migrations",
        ],
    }
}

impl Header {
    /// Validate the top-level definition object, accumulating rejections.
    pub(crate) fn build<'a>(reporter: &mut Reporter, root: &'a DocValue) -> Option<Parsed<'a>> {
        let Some(members) = root.as_object() else {
            reporter.reject(
                root.span,
                code::HEADER,
                "a package definition must be an object",
            );
            return None;
        };

        // §4.1: the language generation is checked before any other member.
        check_generation(reporter, members)?;

        let kind = identify_kind(reporter, root, members)?;
        let identity = read_identity(reporter, members, kind)?;

        let allowed = allowed_members(kind);
        for member in members {
            classify_member(reporter, member, allowed);
        }

        if let Some(semantics) = root.member("$semantics") {
            check_semantics(reporter, &semantics.value);
        }
        if let Some(requires) = root.member("$requires") {
            check_requires(reporter, &requires.value);
        }
        if let Some(resources) = root.member("$resources") {
            check_resources(reporter, &resources.value);
        }
        if let Some(history) = root.member("$history") {
            crate::history::check(reporter, &history.value);
        }
        if let Some(config) = root.member("$config") {
            crate::module::check_config(reporter, &config.value);
        }
        if let Some(uses) = root.member("$use") {
            crate::module::check_use(reporter, &uses.value);
        }
        if let Some(deps) = root.member("$deps") {
            crate::module::check_deps(reporter, &deps.value);
        }
        if let Some(expose) = root.member("$expose") {
            crate::module::check_expose(reporter, &expose.value);
        }
        if root.member("$model").is_none() {
            reporter.reject_hint(
                root.span,
                code::MISSING_MEMBER,
                "a package definition requires a `$model` object",
                "add a `$model` describing the application state",
            );
        }

        Some(Parsed {
            header: Header { kind, identity },
            model: root.member("$model").map(|m| &m.value),
            types: root.member("$types").map(|m| &m.value),
            data: root.member("$data").map(|m| &m.value),
        })
    }
}

/// §4.1: reject an unsupported `$liasse` value before interpreting anything.
fn check_generation(reporter: &mut Reporter, members: &[DocMember]) -> Option<()> {
    let Some(member) = members.iter().find(|m| m.name.text == "$liasse") else {
        reporter.reject_hint(
            members.first().map_or(default_span(), |m| m.span),
            code::MISSING_MEMBER,
            "`$liasse` is required and selects the language generation",
            "add `\"$liasse\": 1`",
        );
        return None;
    };
    match member.value.as_number().and_then(|text| text.parse::<i64>().ok()) {
        Some(SUPPORTED_GENERATION) => Some(()),
        Some(other) => {
            reporter.reject(
                member.value.span,
                code::LANGUAGE,
                format!("`$liasse`: {other} is not a language generation supported by this runtime"),
            );
            None
        }
        None => {
            reporter.reject_hint(
                member.value.span,
                code::LANGUAGE,
                "`$liasse` must be the integer language generation",
                "use `\"$liasse\": 1`",
            );
            None
        }
    }
}

fn identify_kind(reporter: &mut Reporter, root: &DocValue, members: &[DocMember]) -> Option<Kind> {
    let has_app = members.iter().any(|m| m.name.text == "$app");
    let has_module = members.iter().any(|m| m.name.text == "$module");
    match (has_app, has_module) {
        (true, false) => Some(Kind::Application),
        (false, true) => Some(Kind::Module),
        (true, true) => {
            reporter.reject_hint(
                root.span,
                code::HEADER,
                "a package declares exactly one of `$app` and `$module`",
                "keep `$app` for an application or `$module` for a module, not both",
            );
            None
        }
        (false, false) => {
            reporter.reject_hint(
                root.span,
                code::HEADER,
                "a package must declare `$app` or `$module`",
                "add `\"$app\": \"vendor.name@1.0.0\"`",
            );
            None
        }
    }
}

fn read_identity(reporter: &mut Reporter, members: &[DocMember], kind: Kind) -> Option<PackageId> {
    let name = match kind {
        Kind::Application => "$app",
        Kind::Module => "$module",
    };
    let member = members.iter().find(|m| m.name.text == name)?;
    let Some(text) = member.value.as_string() else {
        reporter.reject(
            member.value.span,
            code::HEADER,
            format!("`{name}` must be a `name@version` string"),
        );
        return None;
    };
    match PackageId::parse(text) {
        Ok(identity) => Some(identity),
        Err(reason) => {
            reporter.reject(member.value.span, code::HEADER, reason);
            None
        }
    }
}

/// Classify one top-level member: a reserved declaration outside the grammar, or
/// an application-defined name the closed top-level object does not accept.
fn classify_member(reporter: &mut Reporter, member: &DocMember, allowed: &[&str]) {
    let name = member.name.text.as_str();
    if allowed.contains(&name) {
        return;
    }
    if name.starts_with('$') {
        reporter.reject_hint(
            member.span,
            code::UNKNOWN_MEMBER,
            format!("`{name}` is not part of the package-definition grammar"),
            "remove it, or move a module-only member into a `$module` definition",
        );
    } else {
        reporter.reject_hint(
            member.span,
            code::UNKNOWN_MEMBER,
            format!("top-level member `{name}` is not part of the package grammar"),
            "application state belongs under `$model`",
        );
    }
}

fn check_semantics(reporter: &mut Reporter, value: &DocValue) {
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

fn check_requires(reporter: &mut Reporter, value: &DocValue) {
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

fn check_resources(reporter: &mut Reporter, value: &DocValue) {
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

fn default_span() -> liasse_diag::ByteSpan {
    liasse_diag::ByteSpan::point(0)
}
