//! Module composition declarations (SPEC.md §13, Annex C.15).
//!
//! Validates the static grammar of the module-composition surface:
//!
//! * a `$modules` space carries `$expose?`/`$interfaces?`/`$auth?` (§13.2–13.8);
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

use liasse_expr::{ExprType, RowType};
use liasse_syntax::DocValue;
use liasse_value::Type;

use crate::bucket::node_at_mut;
use crate::doc::DocValueExt;
use crate::report::{code, Reporter};
use crate::resolve::Resolver;
use crate::state::{Node, Shape};

/// Pre-pass (§13.8/§13.9): type each module space's placeholder view node into a
/// keyed view of instances. Each instance shape's interface members are projected
/// through the resolver (so a nested-collection `$view` referencing a `$types`
/// shape resolves), and the row is keyed by the instance name — a non-empty text
/// value that forms the local component of instance identity (§13.3). This lets
/// `.modules::iface` interface aggregation (§13.9) and `modules.$key` type-check
/// against the declared boundary contracts.
pub(crate) fn type_module_spaces(
    resolver: &Resolver,
    root: &mut Shape,
    spaces: &[(Vec<String>, Shape)],
) {
    let mut computed: Vec<(Vec<String>, RowType)> = Vec::new();
    for (path, instance_shape) in spaces {
        let fields = resolver.shape_row(instance_shape);
        let keyed = RowType::new(
            fields.fields().map(|(name, ty)| (name.clone(), ty.clone())).collect::<Vec<_>>(),
            Some(ExprType::scalar(Type::Text)),
        );
        computed.push((path.clone(), keyed));
    }
    for (path, row) in computed {
        if let Some(Node::View(view)) = node_at_mut(root, &path) {
            view.row = row;
        }
    }
}

/// Validate a `$modules` space object.
pub(crate) fn check_space(reporter: &mut Reporter, value: &DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(value.span, code::MODULE, "`$modules` must be a module-space object");
        return;
    };
    for member in members {
        match member.name.text.as_str() {
            "$expose" => check_expose_object(reporter, &member.value),
            "$interfaces" => check_interfaces(reporter, &member.value),
            "$auth" => check_space_auth(reporter, &member.value),
            other => reporter.reject_hint(
                member.span,
                code::MODULE,
                format!("`{other}` is not a `$modules` member"),
                "a module space carries `$expose`, `$interfaces`, and `$auth`",
            ),
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
                "$view" | "$mut" => {}
                other => reporter.reject(
                    member.span,
                    code::MODULE,
                    format!("`{other}` is not an interface member; use `$view` and `$mut`"),
                ),
            }
        }
    }
}

/// A module-space `$auth` maps a child-visible name to a parent authenticator.
fn check_space_auth(reporter: &mut Reporter, value: &DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(value.span, code::MODULE, "a `$modules` `$auth` maps child names to parent authenticators");
        return;
    };
    for member in members {
        if member.value.as_string().is_none() {
            reporter.reject(
                member.value.span,
                code::MODULE,
                format!("`$auth.{}` must name one parent authenticator", member.name.text),
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
                other => reporter.reject(
                    member.span,
                    code::MODULE,
                    format!("`{other}` is not an `$expose` member; use `$view` and `$mut`"),
                ),
            }
        }
    }
}

