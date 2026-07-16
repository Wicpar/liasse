//! The spanned expression / statement AST (SPEC.md §6, §7, Annex C.3-C.9).
//!
//! Every node carries a [`ByteSpan`] locating it in the source text. The tree
//! is pure syntax: no name resolution, typing, precedence of view combinators,
//! or evaluation is performed or implied here. A value of any of these types
//! exists only as the output of [`crate::parse_expression`], so an unparsed or
//! malformed expression is unrepresentable.

use liasse_diag::ByteSpan;

/// A parsed expression source: exactly one top-level statement.
///
/// A bare value/view expression (a `$check`, `$view`, `$normalize`, default,
/// ...) parses as [`StmtKind::Bare`]; a `$mut` statement parses as one of the
/// other [`StmtKind`] variants.
#[derive(Debug, Clone, PartialEq)]
pub struct SpannedExpression {
    /// The single top-level statement.
    pub statement: Stmt,
}

impl SpannedExpression {
    /// The top-level statement.
    #[must_use]
    pub fn statement(&self) -> &Stmt {
        &self.statement
    }

    /// The span covering the whole source.
    #[must_use]
    pub fn span(&self) -> ByteSpan {
        self.statement.span
    }
}

/// A mutation statement or a bare expression, with its span (Annex C.9).
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    /// The bytes this statement covers.
    pub span: ByteSpan,
    /// Which statement form this is.
    pub kind: StmtKind,
}

/// The statement forms of Annex C.9. Forms that are *also* ordinary
/// expressions (`collection + view`, calls, patch blocks, `assert(...)`) parse
/// as [`StmtKind::Bare`]; only the shapes that are not expressions get their
/// own variant.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// `return value_or_view` — the final statement of a program.
    Return(Expr),
    /// `target = value` — assignment / local binding.
    Assign {
        /// The assignment target (a local name, field, or selector).
        target: Expr,
        /// The assigned value or mutation result.
        value: Expr,
    },
    /// `field -` — clear an optional field (trailing minus).
    Clear(Expr),
    /// Any statement that is also a plain expression.
    Bare(Expr),
}

/// A spanned expression node.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    /// The bytes this expression covers.
    pub span: ByteSpan,
    /// The expression form.
    pub kind: ExprKind,
}

/// The expression forms.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// `none` — the absent optional value.
    None,
    /// A boolean literal.
    Bool(bool),
    /// An integer literal, kept as raw base-10 text (Annex A canonical form is
    /// a wire concern, not the parser's).
    Int(String),
    /// A decimal literal, kept as raw text.
    Decimal(String),
    /// A string literal, with escapes decoded.
    Str(String),
    /// A list literal `[a, b, ...]`. The empty list is the empty-view
    /// combinator `[]` (§7.4).
    List(Vec<Expr>),
    /// An object / struct literal `{ ... }` (also a projection block used in
    /// value position, e.g. `+ { title: @title }`).
    Object(Vec<BlockMember>),

    /// `/` — the package root.
    Root,
    /// `.` — the current value or row.
    Current,
    /// `^`, `^^`, ... — a lexical parent scope, by depth (`^` is depth 1).
    Parent(u32),
    /// `#name` — an imported module or parent-surface binding.
    Import(Ident),
    /// `@name` — a mutation or view parameter.
    Param(Ident),
    /// `$name` — a structural binding (`$actor`, `$config`, `$key`, ...).
    Structural(Ident),
    /// A bare `name` — a local or row binding.
    Name(Ident),

    /// `base.field` — field access (`base` is [`Expr::current`] for a leading
    /// `.field`).
    Field {
        /// The receiver.
        base: Box<Expr>,
        /// The accessed member.
        member: Ident,
    },
    /// `base::field` — same-name row binding traversal (§6.4).
    SameName {
        /// The receiver.
        base: Box<Expr>,
        /// The traversed collection member.
        member: Ident,
    },
    /// `base[selector]` — a row selector (§6.3).
    Select {
        /// The collection expression.
        base: Box<Expr>,
        /// The selection.
        selector: Selector,
    },
    /// `callee(args)` — a function or mutation call.
    Call {
        /// The called expression (a name, a `namespace.fn`, or a row.method).
        callee: Box<Expr>,
        /// The arguments.
        args: Vec<Arg>,
    },
    /// `base { ... }` — a projection or patch block (§7.1, §8.6).
    Block {
        /// The source / receiver.
        base: Box<Expr>,
        /// The block members.
        members: Vec<BlockMember>,
    },

    /// A prefix `!` or `-`.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        operand: Box<Expr>,
    },
    /// A CEL binary operator (§6.1) or the `??` view fallback (§7.4).
    Binary {
        /// The operator.
        op: BinaryOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
    },
    /// `cond ? a : b`.
    Ternary {
        /// The condition.
        cond: Box<Expr>,
        /// The value when true.
        then: Box<Expr>,
        /// The value when false.
        otherwise: Box<Expr>,
    },
    /// A flat chain of view combinators `|` / `&` (§7.4). Per SPEC-ISSUES item
    /// 25 the spec pins no relative precedence, associativity, or grouping for
    /// these, so the parser records them flat rather than silently nesting.
    /// `operators.len() == operands.len() - 1`.
    Combination {
        /// The combined operands, in source order.
        operands: Vec<Expr>,
        /// The operators between them, in source order.
        operators: Vec<CombinatorOp>,
    },
}

impl Expr {
    /// A `.` current-value node spanning `span`.
    #[must_use]
    pub fn current(span: ByteSpan) -> Self {
        Self {
            span,
            kind: ExprKind::Current,
        }
    }
}

/// A name with its span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    /// The bytes of the name.
    pub span: ByteSpan,
    /// The name text (without any `#`, `@`, `$`, or `.` sigil).
    pub text: String,
    /// Whether the name was written with a leading `$` (a structural member).
    pub structural: bool,
}

/// A row selector (Annex C.6).
#[derive(Debug, Clone, PartialEq)]
pub enum Selector {
    /// `[k]`, `[a, b, set]` — one or more independent key sources.
    Keys(Vec<Expr>),
    /// `[:name]` or `[:name | condition]` — a bound filtered selection.
    Bind {
        /// The row binding name.
        name: Ident,
        /// The optional filter predicate.
        condition: Option<Box<Expr>>,
    },
}

/// A call argument (Annex C.9).
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    /// A positional argument.
    Positional(Expr),
    /// A `name: value` argument.
    Named {
        /// The parameter name.
        name: Ident,
        /// The argument value.
        value: Expr,
    },
}

/// A member of a projection block, patch block, or object literal
/// (Annex C.7, C.9). The parser keeps projection and patch members in one type
/// because they share `receiver { ... }` syntax; the model layer interprets
/// them by context.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockMember {
    /// The bytes this member covers.
    pub span: ByteSpan,
    /// Which member form this is.
    pub kind: BlockMemberKind,
}

/// The block-member forms.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockMemberKind {
    /// `$key: ...`, `$sort: [...]`, `$skip: n`, `$limit: n`, ... — a structural
    /// projection directive.
    Directive {
        /// The `$`-prefixed directive name.
        name: Ident,
        /// The directive value.
        value: Expr,
    },
    /// `-field` — clear an optional field (patch).
    Clear(Ident),
    /// `name: value`, or a bare `name:` self-binding when `value` is `None`.
    Named {
        /// The output name.
        name: Ident,
        /// The output expression, or `None` for `name:` self-binding.
        value: Option<Expr>,
    },
    /// `field = value` — a patch assignment.
    Assign {
        /// The patched field.
        target: Ident,
        /// The value.
        value: Expr,
    },
    /// A bare expression member: `field`, `@param`, or `binding.field`.
    Shorthand(Expr),
}

/// A prefix operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// `!` logical negation.
    Not,
    /// `-` arithmetic negation.
    Neg,
}

/// A binary operator (CEL scalar operators plus `??`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    /// `||`
    Or,
    /// `&&`
    And,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `in`
    In,
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `??` view / optional fallback.
    Fallback,
}

/// A view combinator (§7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombinatorOp {
    /// `|` union.
    Union,
    /// `&` intersection.
    Intersect,
}
