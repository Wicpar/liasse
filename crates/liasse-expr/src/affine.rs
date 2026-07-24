//! Move-versus-copy value classification and the move-operator rules (SPEC §8.5).
//!
//! Two orthogonal concerns live here:
//!
//! 1. **Affinity** — whether a value is *copyable* (`=` may duplicate it) or
//!    *move-only* (affine: `=` is a type error; only `<-`/`->` transfers it). The
//!    classification is a property of the value's type, delegated to
//!    [`liasse_value::Type::is_copyable`]. It is the single hook a forthcoming
//!    move-only type (a `module` value, a later task) opts into.
//! 2. **Move flow** — the two spellings `dest <- source` and `source -> dest` are
//!    one move that transfers the value and leaves a moved-from *source binding*
//!    unset. Only a move consumes its source; reads, field access, and call
//!    arguments **borrow**. A binding read after it was moved from — before it is
//!    reassigned — is a *use-after-move*. [`MoveTracker`] records which places are
//!    moved-from so a checker (or interpreter) can reject that read loudly.
//!
//! Everything here is pure and host-agnostic, so the model checker (load time) and
//! the runtime interpreter share one implementation, and the move-only rules are
//! unit-tested against a synthetic move-only marker in this crate even though no
//! move-only *type* exists yet.

use std::collections::BTreeSet;

use liasse_syntax::{Expr, ExprKind};

use crate::ty::ExprType;

/// Whether a value may be copied (`=`) or must be moved (`<-`/`->`) — SPEC §8.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affinity {
    /// A copyable value: `=` duplicates it; `<-`/`->` is an explicit transfer.
    Copyable,
    /// A move-only (affine) value: `=` is a type error; only `<-`/`->` transfers
    /// it, leaving its source moved-from.
    MoveOnly,
}

impl Affinity {
    /// Whether this affinity is copyable.
    #[must_use]
    pub fn is_copyable(self) -> bool {
        matches!(self, Self::Copyable)
    }

    /// Whether this affinity is move-only (affine).
    #[must_use]
    pub fn is_move_only(self) -> bool {
        matches!(self, Self::MoveOnly)
    }
}

impl ExprType {
    /// The move/copy classification of a value of this result type (SPEC §8.5).
    ///
    /// A scalar/structured result delegates to [`liasse_value::Type::is_copyable`].
    /// A row or view result is read-only derived data and so is copyable today —
    /// no field type is move-only yet, so no row owns a move-only value; that is a
    /// concern for the move-only `module` type (a later task), at which point a row
    /// carrying a move-only field would classify move-only through the same
    /// component delegation `Type::is_copyable` already performs.
    #[must_use]
    pub fn affinity(&self) -> Affinity {
        let copyable = match self {
            Self::Scalar(ty) => ty.is_copyable(),
            Self::Row(_) | Self::View(_) => true,
        };
        if copyable {
            Affinity::Copyable
        } else {
            Affinity::MoveOnly
        }
    }

    /// Whether `=` may COPY a value of this type (SPEC §8.5). The negation is a
    /// move-only value, which `=` must not duplicate.
    #[must_use]
    pub fn is_copyable(&self) -> bool {
        self.affinity().is_copyable()
    }
}

/// The `=` copy rule (SPEC §8.5): `=` COPIES, so it is valid only when the assigned
/// value is copyable. A move-only value must be transferred with `<-`/`->` instead.
/// Returns `true` when `=` is rejected — i.e. the value is move-only.
#[must_use]
pub fn assignment_rejects_move_only(value: Affinity) -> bool {
    value.is_move_only()
}

/// The canonical key of a *place* expression — a binding or a dotted path — used to
/// track moved-from state, or `None` when the expression is not a simple place
/// (a call, a selector, an arithmetic result, a literal: these own no binding to
/// leave moved-from). A leading `.`/`/` root renders as itself so `.a.b` and `a.b`
/// never collide.
#[must_use]
pub fn place_key(expr: &Expr) -> Option<String> {
    let mut out = String::new();
    render_place(expr, &mut out).then_some(out)
}

fn render_place(expr: &Expr, out: &mut String) -> bool {
    match &expr.kind {
        ExprKind::Name(id) => {
            out.push_str(&id.text);
            true
        }
        ExprKind::Current => {
            out.push('.');
            true
        }
        ExprKind::Root => {
            out.push('/');
            true
        }
        ExprKind::Param(id) => {
            out.push('@');
            out.push_str(&id.text);
            true
        }
        ExprKind::Structural(id) => {
            out.push('$');
            out.push_str(&id.text);
            true
        }
        ExprKind::Import(id) => {
            out.push('#');
            out.push_str(&id.text);
            true
        }
        ExprKind::Field { base, member } => {
            if !render_place(base, out) {
                return false;
            }
            // `.field` (base is `.` or `/`) already carries its separator.
            if !out.ends_with('.') && !out.ends_with('/') {
                out.push('.');
            }
            out.push_str(&member.text);
            true
        }
        _ => false,
    }
}

/// Every simple *place* an expression reads, in traversal order. A maximal place
/// (`a.b.c`) is collected whole; a call, selector, or operator is not a place, so
/// the walk descends into its children and collects the places at their leaves
/// (`f(a).b` collects `a`). Used to detect a read of a moved-from binding.
#[must_use]
pub fn read_places(expr: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    collect_reads(expr, &mut out);
    out
}

fn collect_reads(expr: &Expr, out: &mut Vec<String>) {
    if let Some(key) = place_key(expr) {
        out.push(key);
        return;
    }
    for child in children(expr) {
        collect_reads(child, out);
    }
}

/// The direct sub-expressions of a non-place node, for the read walk. Kept local
/// (rather than reusing the syntax crate's drop-walker, which consumes the tree)
/// so the walk borrows.
fn children(expr: &Expr) -> Vec<&Expr> {
    match &expr.kind {
        ExprKind::Unary { operand, .. } => vec![operand],
        ExprKind::Binary { lhs, rhs, .. } => vec![lhs, rhs],
        ExprKind::Ternary {
            cond,
            then,
            otherwise,
        } => vec![cond, then, otherwise],
        ExprKind::Field { base, .. }
        | ExprKind::SameName { base, .. }
        | ExprKind::Select { base, .. } => {
            let mut v = vec![base.as_ref()];
            if let ExprKind::Select { selector, .. } = &expr.kind {
                v.extend(selector_exprs(selector));
            }
            v
        }
        ExprKind::Call { callee, args } => {
            let mut v = vec![callee.as_ref()];
            v.extend(args.iter().map(arg_expr));
            v
        }
        ExprKind::Block { base, members } => {
            let mut v = vec![base.as_ref()];
            v.extend(members.iter().filter_map(member_expr));
            v
        }
        ExprKind::List(items) => items.iter().collect(),
        ExprKind::Object(members) => members.iter().filter_map(member_expr).collect(),
        ExprKind::Combination { operands, .. } => operands.iter().collect(),
        _ => Vec::new(),
    }
}

fn selector_exprs(selector: &liasse_syntax::Selector) -> Vec<&Expr> {
    match selector {
        liasse_syntax::Selector::Keys(keys) => keys.iter().collect(),
        liasse_syntax::Selector::Bind { condition, .. } => {
            condition.iter().map(Box::as_ref).collect()
        }
    }
}

fn arg_expr(arg: &liasse_syntax::Arg) -> &Expr {
    match arg {
        liasse_syntax::Arg::Positional(expr) | liasse_syntax::Arg::Named { value: expr, .. } => {
            expr
        }
    }
}

fn member_expr(member: &liasse_syntax::BlockMember) -> Option<&Expr> {
    match &member.kind {
        liasse_syntax::BlockMemberKind::Directive { value, .. }
        | liasse_syntax::BlockMemberKind::Assign { value, .. }
        | liasse_syntax::BlockMemberKind::Shorthand(value) => Some(value),
        liasse_syntax::BlockMemberKind::Named { value, .. } => value.as_ref(),
        liasse_syntax::BlockMemberKind::Clear(_) => None,
    }
}

/// Which places are currently moved-from within one mutation program (SPEC §8.5
/// use-after-move). A place is keyed by its canonical [`place_key`]. Only a MOVE
/// records a moved-from place; reads, field access, and call arguments borrow and
/// never record one — that is the move-versus-borrow distinction.
#[derive(Debug, Default, Clone)]
pub struct MoveTracker {
    moved: BTreeSet<String>,
}

impl MoveTracker {
    /// A tracker with nothing moved-from yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `place` was consumed as the source of a move: it is moved-from
    /// until reassigned.
    pub fn consume(&mut self, place: impl Into<String>) {
        self.moved.insert(place.into());
    }

    /// Record that `place` was (re)assigned or written: it — and any path beneath
    /// it — is live again. Reassigning a whole binding revives its sub-paths;
    /// reassigning a sub-path does not revive a moved-from prefix above it.
    pub fn reassign(&mut self, place: &str) {
        self.moved
            .retain(|moved| moved != place && !is_subpath(place, moved));
    }

    /// Whether reading `place` would read a moved-from value: `place` is itself
    /// moved-from, extends a moved-from prefix, or wholly contains a moved-from
    /// sub-path.
    #[must_use]
    pub fn is_moved(&self, place: &str) -> bool {
        self.moved.iter().any(|moved| overlaps(moved, place))
    }

    /// The first place `expr` reads that is currently moved-from, if any — the
    /// use-after-move the caller must reject.
    #[must_use]
    pub fn first_moved_read(&self, expr: &Expr) -> Option<String> {
        read_places(expr)
            .into_iter()
            .find(|place| self.is_moved(place))
    }
}

/// Whether `path` is a strict sub-path of `prefix` at a segment boundary
/// (`a` ⊐ `a.b`; `a` ⊅ `ax`).
fn is_subpath(prefix: &str, path: &str) -> bool {
    path.len() > prefix.len()
        && path.starts_with(prefix)
        && path.as_bytes().get(prefix.len()) == Some(&b'.')
}

/// Whether two dotted place paths overlap: equal, or one a sub-path of the other.
fn overlaps(a: &str, b: &str) -> bool {
    a == b || is_subpath(a, b) || is_subpath(b, a)
}

#[cfg(test)]
mod tests {
    use liasse_diag::SourceMap;
    use liasse_syntax::{StmtKind, parse_expression};
    use liasse_value::Type;

    use super::*;

    type Check = Result<(), String>;

    /// Parse a bare expression for the read/place helpers. Routes failures through
    /// `Result` so the crate's deny-by-default lints (no `unwrap`/`expect`/`panic`)
    /// hold in this unit-test module too.
    fn source(text: &str) -> Result<Expr, String> {
        let mut sources = SourceMap::new();
        let id = sources.add_label("affine", text);
        let parsed = parse_expression(id, text).map_err(|d| d.render(&sources))?;
        match parsed.statement.kind {
            StmtKind::Bare(expr) => Ok(expr),
            other => Err(format!("expected a bare expression, got {other:?}")),
        }
    }

    /// The test-only move-only marker: a synthetic move-only value standing in for
    /// the forthcoming move-only `module` type (none exists yet). Its affinity is
    /// exactly what [`ExprType::affinity`] will report once
    /// [`liasse_value::Type::is_copyable`] returns `false` for that type — the
    /// classification hook the module type opts into.
    const fn move_only_marker() -> Affinity {
        Affinity::MoveOnly
    }

    // --- affinity classification --------------------------------------------

    #[test]
    fn ordinary_result_types_are_copyable() {
        // No move-only type exists yet, so every result classifies copyable.
        assert_eq!(ExprType::scalar(Type::Text).affinity(), Affinity::Copyable);
        assert_eq!(ExprType::scalar(Type::Int).affinity(), Affinity::Copyable);
        assert!(ExprType::scalar(Type::Blob).is_copyable());
    }

    // --- `=` copies, move-only rejects `=` ----------------------------------

    #[test]
    fn assignment_copies_a_copyable_value() {
        // `=` is valid for a copyable value.
        assert!(!assignment_rejects_move_only(Affinity::Copyable));
    }

    #[test]
    fn assignment_rejects_a_move_only_value() {
        // `=` on the move-only marker is a type error; it must be moved instead.
        assert!(assignment_rejects_move_only(move_only_marker()));
    }

    // --- move consumes; read borrows ----------------------------------------

    #[test]
    fn a_move_marks_its_source_moved_from() {
        // `dest <- source`: after the move, `source` is moved-from.
        let mut tracker = MoveTracker::new();
        assert!(!tracker.is_moved("held"), "live before the move");
        tracker.consume("held");
        assert!(tracker.is_moved("held"), "moved-from after the move");
    }

    #[test]
    fn a_read_borrows_and_does_not_consume() -> Check {
        // Reading, field access, and passing as an argument borrow — they never
        // consume — so scanning reads leaves the tracker unchanged. This is the
        // move-versus-borrow distinction: only a move (`consume`) marks moved-from.
        let tracker = MoveTracker::new();
        for text in ["held", "held.field", "f(held)", "held + other"] {
            assert!(
                tracker.first_moved_read(&source(text)?).is_none(),
                "a read of a live binding is not use-after-move: {text}"
            );
        }
        assert!(!tracker.is_moved("held"), "reads must not consume `held`");
        Ok(())
    }

    // --- use-after-move ------------------------------------------------------

    #[test]
    fn reading_a_moved_from_binding_is_use_after_move() -> Check {
        let mut tracker = MoveTracker::new();
        tracker.consume("held");
        // A bare read, a field read beneath it, and a read inside a call all trip.
        assert_eq!(
            tracker.first_moved_read(&source("held")?).as_deref(),
            Some("held")
        );
        assert_eq!(
            tracker.first_moved_read(&source("held.field")?).as_deref(),
            Some("held.field")
        );
        assert_eq!(
            tracker.first_moved_read(&source("f(held)")?).as_deref(),
            Some("held")
        );
        Ok(())
    }

    #[test]
    fn reassigning_a_moved_from_binding_revives_it() -> Check {
        let mut tracker = MoveTracker::new();
        tracker.consume("held");
        assert!(tracker.is_moved("held"));
        tracker.reassign("held");
        assert!(
            !tracker.is_moved("held"),
            "a reassigned binding is readable again"
        );
        assert!(tracker.first_moved_read(&source("held")?).is_none());
        Ok(())
    }

    #[test]
    fn the_move_only_marker_cycle_holds_end_to_end() -> Check {
        // `=` errors, `<-`/`->` (consume) works and marks moved-from, a read
        // borrows, and a read after the move is use-after-move.
        assert!(
            assignment_rejects_move_only(move_only_marker()),
            "`=` errors"
        );
        let mut tracker = MoveTracker::new();
        assert!(
            tracker.first_moved_read(&source("m")?).is_none(),
            "a read borrows"
        );
        tracker.consume("m"); // `dest <- m`
        assert!(tracker.is_moved("m"), "the move consumed `m`");
        assert_eq!(
            tracker.first_moved_read(&source("m")?).as_deref(),
            Some("m"),
            "reading `m` after the move is use-after-move"
        );
        Ok(())
    }

    // --- path overlap --------------------------------------------------------

    #[test]
    fn moved_paths_overlap_by_segment_prefix() {
        let mut tracker = MoveTracker::new();
        tracker.consume("a");
        assert!(tracker.is_moved("a.b"), "extending a moved prefix reads it");
        assert!(
            !tracker.is_moved("ax"),
            "a distinct binding sharing a prefix is fine"
        );
        assert!(!tracker.is_moved("b"), "an unrelated binding is fine");
    }

    #[test]
    fn place_key_renders_paths_and_none_for_non_places() -> Check {
        assert_eq!(place_key(&source("held")?).as_deref(), Some("held"));
        assert_eq!(place_key(&source(".a.b")?).as_deref(), Some(".a.b"));
        assert_eq!(place_key(&source("@p")?).as_deref(), Some("@p"));
        assert!(
            place_key(&source("f(x)")?).is_none(),
            "a call is not a place"
        );
        assert!(
            place_key(&source("a + b")?).is_none(),
            "arithmetic is not a place"
        );
        Ok(())
    }
}
