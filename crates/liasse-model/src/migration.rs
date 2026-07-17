//! Migration declarations (SPEC.md §20, Annex C.15).
//!
//! `$migrations` maps an exact source package version to one ordered atomic
//! migration program — an array of mutation statements over the prospective
//! target state (§20.1). It is a model-root declaration (a sibling of the
//! collections and `$public` inside `$model`). This validates its static shape:
//! each key parses as a `major.minor.patch` version, each program is a non-empty
//! array of statement strings that parse, no statement writes the read-only
//! `$old` source state, and no statement calls a non-deterministic function
//! (§20.1 "MUST use deterministic pure functions"). The parsed program text is
//! retained on the [`Model`](crate::Model) so the runtime can compile and run it.
//!
//! CORE scope: the migration statements read `$old` (source state) and `.` (the
//! prospective target); *typing* them requires both the source and target row
//! models and the reversible round-trip check (`$back($as(x)) == x`, §20.2).
//! Those need the two-model migration runtime and live in `liasse-runtime`; this
//! pass validates syntax and the definition-only §20.1 constraints.

use std::collections::BTreeMap;

use liasse_diag::SourceMap;
use liasse_syntax::{Arg, BlockMember, BlockMemberKind, Expr, ExprKind, Selector, Stmt, StmtKind};

use crate::doc::DocValueExt;
use crate::names::Version;
use crate::report::{code, Reporter};

/// The retained `$migrations` programs of a package (§20.1): the ordered
/// statement texts of each exact source-version migration program, keyed by the
/// canonical `major.minor.patch` source version. The runtime compiles and runs
/// the program whose key matches the active source package version.
#[derive(Debug, Clone, Default)]
pub struct Migrations {
    by_source: BTreeMap<String, Vec<String>>,
}

impl Migrations {
    /// The ordered migration-statement texts declared for source version
    /// `source` (exact `major.minor.patch`), if the package declares a program
    /// for it (§20.1). `None` when no program is keyed to that source.
    #[must_use]
    pub fn program(&self, source: &str) -> Option<&[String]> {
        self.by_source.get(source).map(Vec::as_slice)
    }

    /// Whether the package declares no migration program.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_source.is_empty()
    }
}

/// The canonical `major.minor.patch` key text of a version.
fn canonical(version: &Version) -> String {
    format!("{}.{}.{}", version.major, version.minor, version.patch)
}

/// Validate a `$migrations` object and retain its parsed programs.
pub(crate) fn check(reporter: &mut Reporter, sources: &mut SourceMap, value: &liasse_syntax::DocValue) -> Migrations {
    let mut migrations = Migrations::default();
    let Some(entries) = value.as_object() else {
        reporter.reject_hint(
            value.span,
            code::MIGRATION,
            "`$migrations` maps a source package version to a migration program",
            "e.g. `\"1.4.0\": [\".people = $old.users { id }\"]`",
        );
        return migrations;
    };
    for entry in entries {
        let key = match Version::parse(&entry.name.text) {
            Ok(version) => canonical(&version),
            Err(reason) => {
                reporter.reject_hint(
                    entry.name.span,
                    code::MIGRATION,
                    format!("`{}` is not an exact source package version: {reason}", entry.name.text),
                    "the key is the exact `major.minor.patch` of the source package",
                );
                continue;
            }
        };
        let statements = check_program(reporter, sources, &entry.value);
        if !statements.is_empty() {
            migrations.by_source.insert(key, statements);
        }
    }
    migrations
}

/// Validate one migration program (an ordered statement array) and return its
/// statement texts. A program that fails validation returns an empty vector so
/// the build still reports the fault without retaining a broken program.
fn check_program(reporter: &mut Reporter, sources: &mut SourceMap, value: &liasse_syntax::DocValue) -> Vec<String> {
    let Some(statements) = value.as_array() else {
        reporter.reject_hint(
            value.span,
            code::MIGRATION,
            "a migration program is an array of mutation statements",
            "wrap the statements in an array, even a single one",
        );
        return Vec::new();
    };
    if statements.is_empty() {
        reporter.reject(value.span, code::MIGRATION, "a migration program must have at least one statement");
        return Vec::new();
    }
    let mut texts = Vec::with_capacity(statements.len());
    let mut ok = true;
    for statement in statements {
        let Some(text) = statement.as_string() else {
            reporter.reject(statement.span, code::MIGRATION, "each migration statement is an expression string");
            ok = false;
            continue;
        };
        let sub = sources.add_label("migration", text.to_owned());
        let parsed = match liasse_syntax::parse_expression(sub, text) {
            Ok(parsed) => parsed,
            Err(diags) => {
                reporter.emit_all(diags);
                ok = false;
                continue;
            }
        };
        if !check_statement(reporter, statement.span, parsed.statement()) {
            ok = false;
        }
        texts.push(text.to_owned());
    }
    if ok { texts } else { Vec::new() }
}

/// The definition-only §20.1 constraints on one migration statement: it MUST NOT
/// write the read-only `$old` source state, and it MUST NOT call a
/// non-deterministic function. Returns whether the statement is admissible.
fn check_statement(reporter: &mut Reporter, span: liasse_diag::ByteSpan, stmt: &Stmt) -> bool {
    let mut ok = true;
    if let Some(target) = write_target(stmt)
        && root_binding(target) == Some("old")
    {
        reporter.reject_hint(
            span,
            code::MIGRATION,
            "a migration statement writes `$old`, which is the read-only source state (§20.1)",
            "read `$old` and write the target state (`.`); the source snapshot cannot be mutated",
        );
        ok = false;
    }
    if let Some(name) = nondeterministic_call(stmt) {
        reporter.reject_hint(
            span,
            code::MIGRATION,
            format!("a migration statement calls the non-deterministic `{name}()` (§20.1)"),
            "a migration program MUST use deterministic pure functions of `$old`/`.`",
        );
        ok = false;
    }
    ok
}

/// The write target of a mutating statement — the expression whose row/field the
/// statement changes — or `None` for a pure statement (`return`, a bare view).
/// Insert/delete/patch bare statements expose their receiver so a write rooted at
/// `$old` is caught.
fn write_target(stmt: &Stmt) -> Option<&Expr> {
    match &stmt.kind {
        StmtKind::Assign { target, .. } => Some(target),
        StmtKind::Clear(target) => Some(target),
        StmtKind::Bare(expr) => bare_write_target(expr),
        StmtKind::Return(_) => None,
    }
}

/// The receiver a bare mutating statement writes: `coll + {…}` / `coll - k`
/// (insert/delete), `-selection` (delete), `base { … }` (patch). Any other bare
/// expression is a read and has no write target.
fn bare_write_target(expr: &Expr) -> Option<&Expr> {
    match &expr.kind {
        ExprKind::Binary { lhs, .. } => Some(lhs),
        ExprKind::Unary { operand, .. } => Some(operand),
        ExprKind::Block { base, .. } => Some(base),
        _ => None,
    }
}

/// The root structural binding a target path is anchored at (`$old.users[k].f` →
/// `old`), or `None` when the path roots at `.`, `/`, a name, or a literal.
fn root_binding(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Structural(id) => Some(id.text.as_str()),
        ExprKind::Field { base, .. }
        | ExprKind::SameName { base, .. }
        | ExprKind::Select { base, .. }
        | ExprKind::Block { base, .. } => root_binding(base),
        _ => None,
    }
}

/// The name of a non-deterministic core function called anywhere in `stmt`
/// (§16.1: `uuid()`, `now()`), or `None`. A migration transform MUST be a
/// deterministic pure function (§20.1), so either is a definition-only defect.
///
/// This is the canonical §20.1 determinism classifier. It gates the
/// `$migrations` program statements here, and the runtime reuses it over the
/// local `$from`/`$as`/`$back` transform expressions (`liasse-runtime`'s
/// migration pass) so both migration positions bar the identical generated
/// calls in lockstep.
pub fn nondeterministic_call(stmt: &Stmt) -> Option<&'static str> {
    let mut found = None;
    walk_stmt(stmt, &mut |expr| {
        if let ExprKind::Call { callee, .. } = &expr.kind
            && let ExprKind::Name(id) = &callee.kind
        {
            match id.text.as_str() {
                "uuid" => found = Some("uuid"),
                "now" => found = found.or(Some("now")),
                _ => {}
            }
        }
    });
    found
}

/// Visit every expression node of a statement.
fn walk_stmt(stmt: &Stmt, visit: &mut impl FnMut(&Expr)) {
    match &stmt.kind {
        StmtKind::Return(expr) | StmtKind::Clear(expr) | StmtKind::Bare(expr) => walk_expr(expr, visit),
        StmtKind::Assign { target, value } => {
            walk_expr(target, visit);
            walk_expr(value, visit);
        }
    }
}

/// Visit `expr` and every sub-expression (pre-order).
fn walk_expr(expr: &Expr, visit: &mut impl FnMut(&Expr)) {
    visit(expr);
    match &expr.kind {
        ExprKind::List(items) => items.iter().for_each(|e| walk_expr(e, visit)),
        ExprKind::Object(members) => members.iter().for_each(|m| walk_block_member(m, visit)),
        ExprKind::Field { base, .. } | ExprKind::SameName { base, .. } => walk_expr(base, visit),
        ExprKind::Select { base, selector } => {
            walk_expr(base, visit);
            walk_selector(selector, visit);
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, visit);
            args.iter().for_each(|a| walk_arg(a, visit));
        }
        ExprKind::Block { base, members } => {
            walk_expr(base, visit);
            members.iter().for_each(|m| walk_block_member(m, visit));
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand, visit),
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, visit);
            walk_expr(rhs, visit);
        }
        ExprKind::Ternary { cond, then, otherwise } => {
            walk_expr(cond, visit);
            walk_expr(then, visit);
            walk_expr(otherwise, visit);
        }
        ExprKind::Combination { operands, .. } => operands.iter().for_each(|e| walk_expr(e, visit)),
        _ => {}
    }
}

fn walk_selector(selector: &Selector, visit: &mut impl FnMut(&Expr)) {
    match selector {
        Selector::Keys(keys) => keys.iter().for_each(|e| walk_expr(e, visit)),
        Selector::Bind { condition, .. } => {
            if let Some(condition) = condition {
                walk_expr(condition, visit);
            }
        }
    }
}

fn walk_arg(arg: &Arg, visit: &mut impl FnMut(&Expr)) {
    match arg {
        Arg::Positional(expr) => walk_expr(expr, visit),
        Arg::Named { value, .. } => walk_expr(value, visit),
    }
}

fn walk_block_member(member: &BlockMember, visit: &mut impl FnMut(&Expr)) {
    match &member.kind {
        BlockMemberKind::Directive { value, .. } | BlockMemberKind::Assign { value, .. } => {
            walk_expr(value, visit);
        }
        BlockMemberKind::Named { value: Some(value), .. } => walk_expr(value, visit),
        BlockMemberKind::Shorthand(expr) => walk_expr(expr, visit),
        BlockMemberKind::Named { value: None, .. } | BlockMemberKind::Clear(_) => {}
    }
}
