//! Module composition declarations (SPEC.md §13, Annex C.15).
//!
//! Validates the static grammar of the module-composition surface:
//!
//! * a **module collection** — a map whose `$value` type is `module` — carries
//!   `$interfaces?` (§13.8) and `$expose?` (§13.4) beside the map markers;
//! * a module package's top-level `$config` is an immutable typed struct
//!   (§13.1), `$use` maps handles to `$parent`/parent-surface/peer specs with an
//!   optional `$optional` group (§13.5), `$deps` maps handles to package specs
//!   (§13.6), and `$expose` maps interface names to `{ $view?, $mut? }` (§13.8).
//!
//! CORE scope: this is a *grammar* pass. Peer/parent resolution against an
//! installed sibling set, version compatibility, interface satisfaction
//! (structural view match, mutation contract match), and `$if_module` binding
//! presence all need the module-composition runtime and the installed-package
//! set; they are documented seams. Expression members are parsed for syntax but
//! not typed, since they read `#handle` imports the standalone model lacks.

use liasse_syntax::DocValue;

use crate::doc::DocValueExt;
use crate::mutation::parse_name;
use crate::names::DeclName;
use crate::report::{code, Reporter};
use crate::types::{NamedTypes, TypeParser};

/// The boundary declarations a module collection carries beside its map markers
/// (§13.4/§13.8). They are a property of the collection that holds the modules,
/// not of a separate node kind: a module collection IS a map of `module` values.
const BOUNDARY_MEMBERS: [&str; 2] = ["$interfaces", "$expose"];

/// Validate the boundary declarations of a module collection (a map whose
/// `$value` is `module`).
pub(crate) fn check_boundary(reporter: &mut Reporter, value: &DocValue) {
    if let Some(interfaces) = value.member("$interfaces") {
        check_interfaces(reporter, &interfaces.value);
    }
    if let Some(expose) = value.member("$expose") {
        check_expose_object(reporter, &expose.value);
    }
}

/// Reject a boundary declaration on a collection whose entries are not `module`
/// values. `$interfaces`/`$expose` declare what a *mounted module* must satisfy
/// and what it may import, so they name nothing on an ordinary collection —
/// refused by name rather than silently ignored.
pub(crate) fn reject_boundary_on_non_module(reporter: &mut Reporter, value: &DocValue) {
    for name in BOUNDARY_MEMBERS {
        if let Some(member) = value.member(name) {
            reporter.reject_hint(
                member.span,
                code::MODULE,
                format!(
                    "`{name}` declares a module boundary contract, but this collection's entries \
                     are not `module` values (§13.8)"
                ),
                "declare the collection as a map of modules — `{ \"$key\": \"text\", \"$value\": \"module\" }`",
            );
        }
    }
}

/// `$interfaces` maps a name to `{ $view?: shape, $mut?: { contract: ... } }`.
fn check_interfaces(reporter: &mut Reporter, value: &DocValue) {
    let Some(interfaces) = value.as_object() else {
        reporter.reject(value.span, code::MODULE, "`$interfaces` maps names to boundary contracts");
        return;
    };
    for interface in interfaces {
        let Some(members) = interface.value.as_object() else {
            reporter.reject(
                interface.value.span,
                code::MODULE,
                format!("interface `{}` must be an object", interface.name.text),
            );
            continue;
        };
        for member in members {
            match member.name.text.as_str() {
                // The `$view` shape is built and typed by the state builder
                // ([`crate::build`]) as the interface's read row (§13.8/§13.9).
                "$view" => {}
                "$mut" => check_interface_muts(reporter, &member.value),
                other => reporter.reject(
                    member.span,
                    code::MODULE,
                    format!("`{other}` is not an interface member; use `$view` and `$mut`"),
                ),
            }
        }
    }
}

/// Validate a module-space interface `$mut` contract map (§13.8). Each key is a
/// mutation contract whose name carries an explicit parameter prototype
/// `name({ param: type })`; each value is an object whose only member is the
/// `$return` response shape (an omitted `$return`, i.e. an empty object, declares
/// a response-free mutation). Malformed prototypes and response shapes are
/// rejected here so the boundary contract is well-typed.
///
/// Cross-package seam: checking that a *child's* bound private mutation satisfies
/// this contract — its parameters are a subset the contract supplies, its
/// response conforms to `$return` — needs both the child package's typed
/// mutations and this contract, so it is the composition runtime's install-time
/// check (§13.3/§13.8), not a single-package rule.
fn check_interface_muts(reporter: &mut Reporter, value: &DocValue) {
    let Some(contracts) = value.as_object() else {
        reporter.reject_hint(
            value.span,
            code::MODULE,
            "an interface `$mut` maps contract prototypes to their response shapes",
            "e.g. `{ \"create({ label: text })\": { \"$return\": \"bool\" } }`",
        );
        return;
    };
    for contract in contracts {
        match parse_name(&contract.name.text) {
            Ok((base, _proto)) => {
                if let Err(reason) = DeclName::parse(&base) {
                    reporter.reject(contract.name.span, code::MODULE, reason);
                }
            }
            Err(reason) => reporter.reject_hint(
                contract.name.span,
                code::MODULE,
                reason,
                "declare the contract with an explicit parameter prototype, e.g. `\"create({ label: text })\"` (§13.8)",
            ),
        }
        let Some(body) = contract.value.as_object() else {
            reporter.reject_hint(
                contract.value.span,
                code::MODULE,
                format!("interface mutation `{}` must be an object carrying its `$return` shape", contract.name.text),
                "e.g. `{ \"$return\": \"bool\" }`, or `{}` for a response-free mutation",
            );
            continue;
        };
        for member in body {
            match member.name.text.as_str() {
                "$return" => check_return_shape(reporter, &member.value),
                other => reporter.reject(
                    member.span,
                    code::MODULE,
                    format!("`{other}` is not an interface mutation member; a contract object carries only `$return` (§13.8)"),
                ),
            }
        }
    }
}

/// Validate an interface mutation `$return` response shape (§13.8): a scalar
/// type, a struct, a `{ $ref: target }`, or a row/view response. A scalar or a
/// struct field's declared type must be a well-formed type expression; the ref
/// target and any deeper row/view resolution are composition seams checked once
/// the boundary is bound.
fn check_return_shape(reporter: &mut Reporter, value: &DocValue) {
    if let Some(text) = value.as_string() {
        if let Err(reason) = TypeParser::parse(text.trim(), &NamedTypes::new()) {
            reporter.reject(
                value.span,
                code::MODULE,
                format!("`$return` names a response type, but `{}` is not one: {reason}", text.trim()),
            );
        }
        return;
    }
    let Some(members) = value.as_object() else {
        reporter.reject_hint(
            value.span,
            code::MODULE,
            "`$return` is a response shape: a scalar type, a struct, a `{ $ref: ... }`, or a row/view",
            "e.g. `\"bool\"`, `{ \"id\": \"text\" }`, or `{ \"$ref\": \".templates\" }`",
        );
        return;
    };
    // A `{ $ref: target }` ref response — the target names a row source, resolved
    // (locally or across the bound boundary) by a later pass; only the descriptor
    // shape is checked here.
    if let Some(reference) = members.iter().find(|m| m.name.text == "$ref") {
        if reference.value.as_string().is_none() {
            reporter.reject(reference.value.span, code::MODULE, "`$return` `$ref` names a target row source as a string");
        }
        return;
    }
    // A struct/row response: each field maps a name to a type. A string field
    // type is validated; a `$key`/`$sort` row directive or a nested struct value
    // is accepted structurally (deeper response typing is a composition seam).
    for member in members {
        if member.name.text.starts_with('$') {
            continue;
        }
        if let Some(text) = member.value.as_string()
            && let Err(reason) = TypeParser::parse(text.trim(), &NamedTypes::new())
        {
            reporter.reject(
                member.value.span,
                code::MODULE,
                format!("`$return` field `{}` has an invalid type `{}`: {reason}", member.name.text, text.trim()),
            );
        }
    }
}

/// Validate a top-level module `$config` (an immutable typed struct, §13.1).
pub(crate) fn check_config(reporter: &mut Reporter, value: &DocValue) {
    if value.as_object().is_none() {
        reporter.reject_hint(
            value.span,
            code::MODULE,
            "`$config` is an immutable typed struct of installation values",
            "declare `$config` as an object of typed fields",
        );
    }
}

/// Validate a top-level `$use` object (§13.5).
pub(crate) fn check_use(reporter: &mut Reporter, value: &DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(value.span, code::MODULE, "`$use` maps handles to bindings");
        return;
    };
    for member in members {
        if member.name.text == "$optional" {
            check_use_group(reporter, &member.value);
            continue;
        }
        check_use_binding(reporter, member);
    }
}

fn check_use_group(reporter: &mut Reporter, value: &DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(value.span, code::MODULE, "`$use.$optional` maps handles to peer specs");
        return;
    };
    for member in members {
        check_use_binding(reporter, member);
    }
}

fn check_use_binding(reporter: &mut Reporter, member: &liasse_syntax::DocMember) {
    if member.value.as_string().is_none() {
        reporter.reject_hint(
            member.value.span,
            code::MODULE,
            format!("`$use.{}` must be a `$parent`, parent-surface, or peer spec string", member.name.text),
            "e.g. `\"people\": \"acme.people/people@1\"` or `\"company\": \"$parent\"`",
        );
    }
}

/// Validate a top-level `$deps` object (§13.6).
pub(crate) fn check_deps(reporter: &mut Reporter, value: &DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(value.span, code::MODULE, "`$deps` maps handles to package specs");
        return;
    };
    for member in members {
        if member.value.as_string().is_none() {
            reporter.reject(
                member.value.span,
                code::MODULE,
                format!("`$deps.{}` must name a `package@major` spec", member.name.text),
            );
        }
    }
}

/// Validate a top-level `$expose` object (§13.8).
pub(crate) fn check_expose(reporter: &mut Reporter, value: &DocValue) {
    check_expose_object(reporter, value);
}

/// Validate an `$if_module` guard value (§13.7): a non-empty string naming one
/// optional `$use` handle. Whether that handle exists under `$use.$optional` and
/// is bound to an enabled compatible instance is resolved by the composition
/// runtime at install/enable/disable time (§13.7), not by this grammar pass.
pub(crate) fn check_if_module(reporter: &mut Reporter, value: &DocValue) {
    match value.as_string() {
        Some(text) if !text.trim().is_empty() => {}
        _ => reporter.reject_hint(
            value.span,
            code::MODULE,
            "`$if_module` names one optional `$use` handle as a non-empty string",
            "e.g. `\"$if_module\": \"billing\"` referencing a handle under `$use.$optional`",
        ),
    }
}

fn check_expose_object(reporter: &mut Reporter, value: &DocValue) {
    let Some(interfaces) = value.as_object() else {
        reporter.reject(value.span, code::MODULE, "`$expose` maps interface names to bound surfaces");
        return;
    };
    for interface in interfaces {
        let Some(members) = interface.value.as_object() else {
            reporter.reject(
                interface.value.span,
                code::MODULE,
                format!("exposed interface `{}` must be an object", interface.name.text),
            );
            continue;
        };
        for member in members {
            match member.name.text.as_str() {
                "$view" | "$mut" => {}
                // §13.7: an exposure MAY be guarded by `$if_module`, naming one
                // optional `$use` handle whose presence makes the exposure active.
                "$if_module" => check_if_module(reporter, &member.value),
                other => reporter.reject(
                    member.span,
                    code::MODULE,
                    format!("`{other}` is not an `$expose` member; use `$view`, `$mut`, and `$if_module`"),
                ),
            }
        }
    }
}

