//! The `$requires` namespace descriptors and `$resources` descriptor objects
//! (SPEC.md §4.1, §16.2). The CORE pass validates `$requires` for *shape* and
//! for the §16.2 single-meaning-per-identifier discipline (no core-namespace or
//! model-declaration collision, every declared requirement used); resolving a
//! namespace *contract* against a host is the runtime's later job.

use std::collections::BTreeSet;

use liasse_diag::SourceMap;
use liasse_syntax::{parse_expression, DocMember, DocValue, DocValueKind, Expr, ExprKind};

use crate::doc::DocValueExt;
use crate::report::{code, Reporter};

/// The core namespace names (§16.1). A package declares only the *non-core*
/// namespaces it uses, so a `$requires` key naming one of these is rejected —
/// it would silently rebind a trusted core namespace to an external contract.
const CORE_NAMESPACES: &[&str] = &[
    "language", "hex", "base64", "sha", "string", "convert", "time",
];

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
        // §2.5/§4.1: a `$requires` key is an application-defined namespace
        // handle, so it must be a valid declaration name — not a `$`-reserved
        // key smuggling a requirement into structural-member space.
        if let Err(reason) = crate::names::DeclName::parse(&member.name.text) {
            reporter.reject_hint(
                member.name.span,
                code::HEADER,
                format!("`$requires` key `{}` is not a valid namespace handle: {reason}", member.name.text),
                "name the requirement with a declaration name, e.g. `\"cbor\": \"liasse.cbor@1\"`",
            );
        } else if CORE_NAMESPACES.contains(&member.name.text.as_str()) {
            // §16.2: "A package declares only the non-core namespaces it uses."
            // A `$requires` key equal to a core namespace name (§16.1) would let
            // an external contract shadow the trusted core namespace at every
            // call site, so every bare identifier stays single-valued.
            reporter.reject_hint(
                member.name.span,
                code::HEADER,
                format!("`$requires` key `{}` names the core namespace `{}` (§16.1)", member.name.text, member.name.text),
                "packages declare only non-core namespaces; choose a different alias",
            );
        }
        if member.value.as_string().is_none() {
            reporter.reject(
                member.value.span,
                code::HEADER,
                format!("`$requires.{}` must be a `namespace@major` string", member.name.text),
            );
        }
    }
}

/// The §16.2 single-meaning discipline that spans `$requires` and the model: a
/// requirement's local key resolves *by contract* (the runtime's job), so the
/// key is purely the expression alias and MUST name exactly one thing.
///
/// - **Collision.** A `$requires` key equal to a top-level `$model` declaration
///   name makes a bare `key.member(...)` head ambiguous between the namespace
///   function and a member access on the declaration, so it is rejected (the
///   core-namespace collision is caught in [`check_requires`]).
/// - **Unused.** Every declared requirement MUST be used — invoked by at least
///   one expression call site (a `key.fn(...)` head anywhere in the model,
///   surfaces, `$expose`, or `$migrations`) or supplying a named value type the
///   model references. An unused-but-declared requirement pins a dead descriptor
///   into the install record, so it is rejected.
pub(super) fn check_requirement_use(reporter: &mut Reporter, root: &DocValue) {
    let Some(requires) = root.member("$requires").map(|m| &m.value) else { return };
    let Some(entries) = requires.as_object() else { return };
    // A package with no `$model` is already rejected; skip the cross-check
    // rather than flag every requirement as unused against an absent model.
    if root.member("$model").is_none() {
        return;
    }

    // Only well-formed, non-core keys reach the cross-check; the malformed and
    // core-collision cases are already rejected in `check_requires`.
    let model_names = top_level_declaration_names(root);
    let used = collect_namespace_heads(root);

    for entry in entries {
        let key = entry.name.text.as_str();
        if crate::names::DeclName::parse(key).is_err() || CORE_NAMESPACES.contains(&key) {
            continue;
        }
        if model_names.contains(key) {
            reporter.reject_hint(
                entry.name.span,
                code::HEADER,
                format!("`$requires` key `{key}` collides with the top-level model declaration `{key}` (§16.2)"),
                "a bare identifier names exactly one thing; rename the requirement alias or the declaration",
            );
        } else if !used.contains(key) {
            reporter.reject_hint(
                entry.name.span,
                code::HEADER,
                format!("`$requires` declares `{key}` but no expression uses it (§16.2)"),
                "remove the unused requirement, or call one of its functions",
            );
        }
    }
}

/// The non-`$` top-level declaration names of `$model` — the bare-identifier
/// expression space a `$requires` alias must stay distinct from (§16.2).
fn top_level_declaration_names(root: &DocValue) -> BTreeSet<String> {
    root.member("$model")
        .and_then(|m| m.value.as_object())
        .into_iter()
        .flatten()
        .map(|m| m.name.text.clone())
        .filter(|name| !name.starts_with('$'))
        .collect()
}

/// Every identifier used as a namespace-call head (`name.member`) anywhere in
/// the package's expression-bearing declarations. The walk parses each string
/// scalar (best effort) and records `Field` receivers spelled as a bare name;
/// `$data` seed values and the header scalars carry no expressions and are
/// excluded so a literal that resembles an access cannot be miscounted.
fn collect_namespace_heads(root: &DocValue) -> BTreeSet<String> {
    /// Header/seed members that carry no host-callable expression.
    const SKIP: &[&str] =
        &["$liasse", "$app", "$module", "$semantics", "$requires", "$resources", "$data"];
    let mut heads = BTreeSet::new();
    let mut sources = SourceMap::new();
    if let Some(members) = root.as_object() {
        for member in members {
            if SKIP.contains(&member.name.text.as_str()) {
                continue;
            }
            collect_from_value(&member.value, &mut heads, &mut sources);
        }
    }
    heads
}

/// Recurse a document value, parsing every string scalar as an expression and
/// harvesting its namespace-call heads.
fn collect_from_value(value: &DocValue, heads: &mut BTreeSet<String>, sources: &mut SourceMap) {
    match &value.kind {
        DocValueKind::String(text) => {
            // A computed value / `$view` writes its expression with a leading `=`
            // marker (`"= u.g(.)"`); strip it so the body parses as an expression.
            let body = text.trim_start().strip_prefix('=').unwrap_or(text);
            let id = sources.add_label("use", body.to_owned());
            if let Ok(parsed) = parse_expression(id, body) {
                walk_heads(crate::check::statement_expr(&parsed), heads);
            }
        }
        DocValueKind::Object(members) => {
            for member in members {
                collect_from_value(&member.value, heads, sources);
            }
        }
        DocValueKind::Array(items) => {
            for item in items {
                collect_from_value(item, heads, sources);
            }
        }
        _ => {}
    }
}

/// Record every `Field` receiver spelled as a bare name (a `name.member` head),
/// recursing into the expression's children.
fn walk_heads(expr: &Expr, heads: &mut BTreeSet<String>) {
    if let ExprKind::Field { base, .. } = &expr.kind
        && let ExprKind::Name(name) = &base.kind
    {
        heads.insert(name.text.clone());
    }
    for child in crate::walk::child_exprs(expr) {
        walk_heads(child, heads);
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
