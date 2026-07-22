//! Bounding the *node-nesting depth* of a parsed expression tree.
//!
//! The pre-parse bracket scan ([`crate::scan`]) keeps `pest`'s recursive descent
//! from overflowing on deeply *bracketed* input, but it counts only `([{`.
//! Bracket-free chains — a unary run `!!!!x`, a field chain `.a.a.a`, a same-name
//! traversal `.a::b::c`, a binary chain `x+x+x` — carry no bracket, so the scan
//! admits them however deep. `pest` parses and the builder lowers such chains
//! *iteratively* (the grammar gathers each with a `*` repetition, not recursion),
//! so no stack grows there; but the resulting AST is genuinely that deep, and the
//! recursive `liasse-expr` checker (`check`/`check_unary`/`check_binary`/…) and
//! evaluator walk it one stack frame per node — overflowing the stack (SIGABRT)
//! far below `pest`'s own limit. This measures the true node depth once, at the
//! parse boundary, so a returned [`SpannedExpression`] is proof its tree is
//! shallow enough for those recursive walks.
//!
//! The walk is itself *iterative* (an explicit work stack): the tree it inspects
//! may be tens of thousands of nodes deep, so a recursive measurement would
//! overflow on exactly the input it exists to reject.

use liasse_diag::ByteSpan;

use crate::expr::ast::{
    Arg, BlockMember, BlockMemberKind, Expr, ExprKind, Selector, SpannedExpression, Stmt, StmtKind,
};

/// One pending node and the nesting depth at which it sits.
type Pending<'a> = (&'a Expr, usize);

impl SpannedExpression {
    /// The depth of the deepest node in the tree, paired with that node's span.
    /// The top-level expression is depth 1 and every child sits one deeper, so
    /// the value equals the maximum `check`/eval recursion this tree drives.
    ///
    /// Computed with an explicit work stack — never recursion — so measuring a
    /// pathologically deep tree cannot itself overflow.
    #[must_use]
    pub(crate) fn deepest(&self) -> (usize, ByteSpan) {
        let mut stack: Vec<Pending<'_>> = Vec::new();
        self.statement.push_roots(&mut stack);
        let mut best = (0usize, self.statement.span);
        while let Some((expr, depth)) = stack.pop() {
            if depth > best.0 {
                best = (depth, expr.span);
            }
            expr.push_children(depth, &mut stack);
        }
        best
    }

    /// Free a tree of arbitrary depth without recursion. The derived `Drop` of a
    /// `Box<Expr>` chain recurses one frame per level, so dropping a rejected
    /// pathologically deep tree (thousands of `!`/`.`/`+` nodes) overflows the
    /// stack just as its checker walk would. This moves every node onto a heap
    /// work list and drops each shell only after its children have been detached,
    /// so no drop ever nests. Used to discard a tree the depth guard rejects; a
    /// tree that passes the guard is shallow and drops normally.
    pub(crate) fn drain(self) {
        let mut stack: Vec<Expr> = Vec::new();
        self.statement.kind.drain_into(&mut stack);
        while let Some(expr) = stack.pop() {
            expr.kind.drain_into(&mut stack);
        }
    }
}

impl Stmt {
    /// Seed the walk with every top-level expression of this statement at depth 1.
    fn push_roots<'a>(&'a self, stack: &mut Vec<Pending<'a>>) {
        match &self.kind {
            StmtKind::Return(expr) | StmtKind::Clear(expr) | StmtKind::Bare(expr) => {
                stack.push((expr, 1));
            }
            StmtKind::Assign { target, value } => {
                stack.push((target, 1));
                stack.push((value, 1));
            }
        }
    }
}

impl Expr {
    /// Push every direct sub-expression of this node at `depth + 1`.
    fn push_children<'a>(&'a self, depth: usize, stack: &mut Vec<Pending<'a>>) {
        let child = depth + 1;
        match &self.kind {
            ExprKind::None
            | ExprKind::Bool(_)
            | ExprKind::Int(_)
            | ExprKind::Decimal(_)
            | ExprKind::Str(_)
            | ExprKind::Root
            | ExprKind::Current
            | ExprKind::Parent(_)
            | ExprKind::Import(_)
            | ExprKind::Param(_)
            | ExprKind::Structural(_)
            | ExprKind::Name(_) => {}
            ExprKind::List(items) | ExprKind::Combination { operands: items, .. } => {
                for item in items {
                    stack.push((item, child));
                }
            }
            ExprKind::Object(members) => Self::push_members(members, child, stack),
            ExprKind::Field { base, .. } | ExprKind::SameName { base, .. } => {
                stack.push((base, child));
            }
            ExprKind::Unary { operand, .. } => stack.push((operand, child)),
            ExprKind::Select { base, selector } => {
                stack.push((base, child));
                selector.push_children(child, stack);
            }
            ExprKind::Call { callee, args } => {
                stack.push((callee, child));
                for arg in args {
                    arg.push_child(child, stack);
                }
            }
            ExprKind::Block { base, members } => {
                stack.push((base, child));
                Self::push_members(members, child, stack);
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                stack.push((lhs, child));
                stack.push((rhs, child));
            }
            ExprKind::Ternary { cond, then, otherwise } => {
                stack.push((cond, child));
                stack.push((then, child));
                stack.push((otherwise, child));
            }
        }
    }

    fn push_members<'a>(members: &'a [BlockMember], depth: usize, stack: &mut Vec<Pending<'a>>) {
        for member in members {
            member.push_child(depth, stack);
        }
    }
}

impl Selector {
    fn push_children<'a>(&'a self, depth: usize, stack: &mut Vec<Pending<'a>>) {
        match self {
            Selector::Keys(keys) => {
                for key in keys {
                    stack.push((key, depth));
                }
            }
            Selector::Bind { condition, .. } => {
                if let Some(condition) = condition {
                    stack.push((condition, depth));
                }
            }
        }
    }
}

impl Arg {
    fn push_child<'a>(&'a self, depth: usize, stack: &mut Vec<Pending<'a>>) {
        match self {
            Arg::Positional(value) | Arg::Named { value, .. } => stack.push((value, depth)),
        }
    }
}

impl BlockMember {
    fn push_child<'a>(&'a self, depth: usize, stack: &mut Vec<Pending<'a>>) {
        match &self.kind {
            BlockMemberKind::Clear(_) => {}
            BlockMemberKind::Named { value, .. } => {
                if let Some(value) = value {
                    stack.push((value, depth));
                }
            }
            BlockMemberKind::Directive { value, .. }
            | BlockMemberKind::Assign { value, .. }
            | BlockMemberKind::Shorthand(value) => stack.push((value, depth)),
        }
    }
}

// --- iterative teardown (`SpannedExpression::drain`) ---
//
// Each `drain_into` *consumes* its node and moves every owned child expression
// onto the shared work list. A node's shell therefore drops only after its
// children have been detached, so the derived `Drop` never recurses.

impl StmtKind {
    fn drain_into(self, stack: &mut Vec<Expr>) {
        match self {
            StmtKind::Return(expr) | StmtKind::Clear(expr) | StmtKind::Bare(expr) => {
                stack.push(expr);
            }
            StmtKind::Assign { target, value } => {
                stack.push(target);
                stack.push(value);
            }
        }
    }
}

impl ExprKind {
    fn drain_into(self, stack: &mut Vec<Expr>) {
        match self {
            ExprKind::None
            | ExprKind::Bool(_)
            | ExprKind::Int(_)
            | ExprKind::Decimal(_)
            | ExprKind::Str(_)
            | ExprKind::Root
            | ExprKind::Current
            | ExprKind::Parent(_)
            | ExprKind::Import(_)
            | ExprKind::Param(_)
            | ExprKind::Structural(_)
            | ExprKind::Name(_) => {}
            ExprKind::List(items) | ExprKind::Combination { operands: items, .. } => {
                stack.extend(items);
            }
            ExprKind::Object(members) => {
                for member in members {
                    member.drain_into(stack);
                }
            }
            ExprKind::Field { base, .. } | ExprKind::SameName { base, .. } => stack.push(*base),
            ExprKind::Unary { operand, .. } => stack.push(*operand),
            ExprKind::Select { base, selector } => {
                stack.push(*base);
                selector.drain_into(stack);
            }
            ExprKind::Call { callee, args } => {
                stack.push(*callee);
                for arg in args {
                    arg.drain_into(stack);
                }
            }
            ExprKind::Block { base, members } => {
                stack.push(*base);
                for member in members {
                    member.drain_into(stack);
                }
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                stack.push(*lhs);
                stack.push(*rhs);
            }
            ExprKind::Ternary { cond, then, otherwise } => {
                stack.push(*cond);
                stack.push(*then);
                stack.push(*otherwise);
            }
        }
    }
}

impl Selector {
    fn drain_into(self, stack: &mut Vec<Expr>) {
        match self {
            Selector::Keys(keys) => stack.extend(keys),
            Selector::Bind { condition, .. } => {
                if let Some(condition) = condition {
                    stack.push(*condition);
                }
            }
        }
    }
}

impl Arg {
    fn drain_into(self, stack: &mut Vec<Expr>) {
        match self {
            Arg::Positional(value) | Arg::Named { value, .. } => stack.push(value),
        }
    }
}

impl BlockMember {
    fn drain_into(self, stack: &mut Vec<Expr>) {
        match self.kind {
            BlockMemberKind::Clear(_) => {}
            BlockMemberKind::Named { value, .. } => {
                if let Some(value) = value {
                    stack.push(value);
                }
            }
            BlockMemberKind::Directive { value, .. }
            | BlockMemberKind::Assign { value, .. }
            | BlockMemberKind::Shorthand(value) => stack.push(value),
        }
    }
}
