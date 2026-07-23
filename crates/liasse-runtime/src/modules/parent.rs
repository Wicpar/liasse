//! §13.4 parent-surface mutation routing.
//!
//! A module child MAY bind an exposed interface mutation to a parent-provided
//! surface mutation rather than to one of its own (§13.4): the `$expose`
//! interface `info` binds `rename_company: "#company.rename({ name: @name })"`,
//! delegating entirely to the `company` parent surface's `rename` mutation. When
//! a parent routes a call to that exposed mutation (§13.10/§13.11), the effect
//! lands on the **parent** row the space is scoped to, not on the child — so the
//! host admits it against its root engine at the module space's containing row.
//!
//! This module parses such a binding into the parts the host needs to build that
//! root call: the imported handle (`company`), the parent mutation contract name
//! (`rename`), and how each parent parameter draws its value from the child
//! call's arguments.

use liasse_diag::SourceMap;
use liasse_syntax::{parse_expression, Arg, Expr, ExprKind, StmtKind};

/// How one parent-mutation parameter draws its value (§13.4). CORE routes the
/// `#handle.mut({ p: @a })` form, where each parameter is fed a child call
/// argument; a literal or computed argument is a documented seam.
pub(crate) enum ArgSource {
    /// The value of the child call's `@name` argument.
    Param(String),
}

/// A parsed `$expose` `$mut` binding that delegates to a parent-surface mutation
/// (`#company.rename({ name: @name })`, §13.4): the imported handle, the parent
/// mutation contract name, and each parent parameter's source.
pub(crate) struct ParentMutationBinding {
    /// The imported parent-surface handle (`company`).
    pub(crate) handle: String,
    /// The parent surface's mutation contract name (`rename`).
    pub(crate) mutation: String,
    /// The `(parent parameter, source)` argument mapping.
    pub(crate) args: Vec<(String, ArgSource)>,
}

impl ParentMutationBinding {
    /// Parse a `#handle.mutation({ p: @a, … })` binding (§13.4), or `None` when the
    /// binding is not a parent-surface mutation delegation (a child root-mutation
    /// reference, an inline program, or an unsupported argument form the CORE route
    /// does not handle).
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut sources = SourceMap::new();
        let source = sources.add_label("parent-mut", text.to_owned());
        let parsed = parse_expression(source, text).ok()?;
        let StmtKind::Bare(expr) = &parsed.statement().kind else {
            return None;
        };
        let ExprKind::Call { callee, args } = &expr.kind else {
            return None;
        };
        let ExprKind::Field { base, member } = &callee.kind else {
            return None;
        };
        let ExprKind::Import(handle) = &base.kind else {
            return None;
        };
        Some(Self {
            handle: handle.text.clone(),
            mutation: member.text.clone(),
            args: parse_args(args)?,
        })
    }
}

/// Parse the call arguments into a `(parent parameter ← child argument)` mapping.
/// Two forms are accepted (§13.4 CORE): direct named arguments `mut(name: @name)`
/// and a single object argument `mut({ name: @name })`; each value MUST be a bare
/// `@param`. Anything else (a positional non-object, a literal, a computed value)
/// is unsupported and yields `None`.
fn parse_args(args: &[Arg]) -> Option<Vec<(String, ArgSource)>> {
    // A single positional object argument `({ p: @a })`.
    if let [Arg::Positional(object)] = args
        && let ExprKind::Object(members) = &object.kind
    {
        let mut out = Vec::new();
        for member in members {
            let liasse_syntax::BlockMemberKind::Named { name, value: Some(value) } = &member.kind else {
                return None;
            };
            out.push((name.text.clone(), param_source(value)?));
        }
        return Some(out);
    }
    // Direct named arguments `(p: @a, …)`.
    let mut out = Vec::new();
    for arg in args {
        let Arg::Named { name, value } = arg else {
            return None;
        };
        out.push((name.text.clone(), param_source(value)?));
    }
    Some(out)
}

/// The [`ArgSource`] of a `@param` argument value, or `None` for any other form.
fn param_source(value: &Expr) -> Option<ArgSource> {
    match &value.kind {
        ExprKind::Param(name) => Some(ArgSource::Param(name.text.clone())),
        _ => None,
    }
}
