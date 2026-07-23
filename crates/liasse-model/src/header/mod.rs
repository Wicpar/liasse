//! The package header (SPEC.md §4, Annex C.1, Annex E).
//!
//! Validates the top-level definition object: the required `$liasse` language
//! generation (checked first, §4.1), the exclusive `$app`/`$module` identity,
//! and the closed Annex C.1 member grammar — any member outside it, including a
//! reserved `$`-member that is not a defined declaration, is rejected (§2.5).
//! The `$semantics` choices live in [`semantics`], the `$requires`/`$resources`
//! descriptors in [`resources`].
//!
//! CORE scope: `$requires` is validated for *shape* only; resolving a namespace
//! against a host and verifying resource digests belong to a later pass, so
//! those are not performed here.

mod resources;
mod semantics;

use liasse_syntax::{DocMember, DocValue};

use crate::doc::DocValueExt;
use crate::names::PackageId;
use crate::report::{code, Reporter};

use resources::{check_requirement_use, check_requires, check_resources};
use semantics::check_semantics;

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
            "$seed",
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
            "$seed",
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
        // §16.2 single-meaning discipline: a `$requires` alias must not collide
        // with a core namespace or a top-level model declaration, and every
        // declared requirement must be used. Spans `$requires` and `$model`.
        check_requirement_use(reporter, root);
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
        // §13.1: an application always describes its state, so `$model` is required.
        // A module owns private state through `$model`, but a pure-consumer module —
        // one that only re-exposes peer/parent capabilities (§13.4/§13.5) and owns no
        // state of its own — legitimately declares none, so `$model` is optional for a
        // module and its absence is an empty state shape (no collections).
        if kind == Kind::Application && root.member("$model").is_none() {
            reporter.reject_hint(
                root.span,
                code::MISSING_MEMBER,
                "an application definition requires a `$model` object",
                "add a `$model` describing the application state",
            );
        }
        // §4.1: `$data` is an alias of `$seed`; declaring both is rejected so a
        // package has exactly one apply-if-absent seed source.
        let data = match (root.member("$data"), root.member("$seed")) {
            (Some(_), Some(seed)) => {
                reporter.reject_hint(
                    seed.span,
                    code::HEADER,
                    "`$data` is an alias of `$seed`; a package declares at most one of the two (§4.1)",
                    "keep the values under a single `$seed` member",
                );
                None
            }
            (data, seed) => data.or(seed).map(|m| &m.value),
        };

        Some(Parsed {
            header: Header { kind, identity },
            model: root.member("$model").map(|m| &m.value),
            types: root.member("$types").map(|m| &m.value),
            data,
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
    // §4.1 `$bundle` (package-authoritative, three-way-merged data) is specified
    // but not yet built here; reject it loudly rather than accept and silently
    // drop it — an honest follow-on hole, not a grammar violation.
    if name == "$bundle" {
        reporter.reject_hint(
            member.span,
            code::UNKNOWN_MEMBER,
            "`$bundle` (§4.1 package-authoritative data) is not supported by this implementation yet",
            "use `$seed` for apply-if-absent starting data until bundle support lands",
        );
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

fn default_span() -> liasse_diag::ByteSpan {
    liasse_diag::ByteSpan::point(0)
}
