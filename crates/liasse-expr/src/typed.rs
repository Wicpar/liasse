//! The typed expression tree: the checker's output and the evaluator's input.
//!
//! A [`TypedExpr`] exists only as the result of
//! [`check`](crate::check::check_expression); every node carries its resolved
//! [`ExprType`] and a [`TypedKind`] whose operator/function/selector choice is
//! already made. Evaluation therefore never re-checks a type or re-dispatches
//! an overload — it matches the resolved kind. This makes "evaluation takes a
//! `TypedExpr`, not raw AST" a type-level guarantee, not a convention.
//!
//! # Recursion bound
//!
//! Both the checker (building this tree) and the evaluator (walking it) recurse
//! structurally on the AST. liasse-syntax caps expression nesting at 512 before
//! this crate ever sees a tree (`scan::check_nesting_depth`), so that cap is the
//! recursion bound here; there is no self-referential or growth path that adds
//! depth beyond the parsed structure.

use liasse_diag::ByteSpan;
use liasse_value::Value;

use crate::ty::ExprType;

/// A type-checked expression: its span, its resolved result type, and its
/// resolved operation.
#[derive(Debug, Clone)]
pub struct TypedExpr {
    span: ByteSpan,
    ty: ExprType,
    kind: TypedKind,
}

impl TypedExpr {
    /// Assemble a checked node. Only the checker calls this.
    #[must_use]
    pub(crate) fn new(span: ByteSpan, ty: ExprType, kind: TypedKind) -> Self {
        Self { span, ty, kind }
    }

    /// The source span.
    #[must_use]
    pub fn span(&self) -> ByteSpan {
        self.span
    }

    /// The resolved result type.
    #[must_use]
    pub fn ty(&self) -> &ExprType {
        &self.ty
    }

    /// The resolved operation.
    #[must_use]
    pub(crate) fn kind(&self) -> &TypedKind {
        &self.kind
    }
}

/// A resolved operation. Overloads (`+` as int/decimal/text, `-` as
/// arithmetic/difference, `==` on scalars vs refs) are already decided.
#[derive(Debug, Clone)]
pub(crate) enum TypedKind {
    /// A constant scalar (`1`, `"x"`, `true`, `none`, a `[]`/`{}` literal value).
    Literal(Value),
    /// The package root `/`.
    Root,
    /// The current value or row `.`.
    Current,
    /// A lexical parent `^`… at the given depth.
    Parent(u32),
    /// `@name`.
    Param(String),
    /// `$name`.
    Structural(String),
    /// `#name`.
    Import(String),
    /// A lexical binding `name` resolved from the base scope.
    ScopeBinding(String),
    /// A row binding `name` introduced within the expression (filter, `::`,
    /// projection member); resolved from the evaluator's local frames.
    LocalBinding(String),
    /// `base.field` field access.
    Field {
        base: Box<TypedExpr>,
        name: String,
    },
    /// `base[selector]` row selection.
    Select {
        base: Box<TypedExpr>,
        selector: TypedSelector,
    },
    /// `base::member` same-name traversal (§6.4), flattening `member` across the
    /// rows of `base` and binding each level to its field name.
    Traverse {
        base: Box<TypedExpr>,
        member: String,
    },
    /// Integer or decimal arithmetic, or `+` text concatenation.
    Arith {
        op: ArithOp,
        class: NumClass,
        lhs: Box<TypedExpr>,
        rhs: Box<TypedExpr>,
    },
    /// Prefix arithmetic negation.
    Neg {
        class: NumClass,
        operand: Box<TypedExpr>,
    },
    /// A comparison, evaluated through the Annex B total order.
    Compare {
        op: CmpOp,
        lhs: Box<TypedExpr>,
        rhs: Box<TypedExpr>,
    },
    /// `&&` / `||` with short-circuit.
    Logic {
        op: LogicOp,
        lhs: Box<TypedExpr>,
        rhs: Box<TypedExpr>,
    },
    /// Prefix `!`.
    Not(Box<TypedExpr>),
    /// `lhs in rhs` — membership in a set or view.
    In {
        needle: Box<TypedExpr>,
        haystack: Box<TypedExpr>,
    },
    /// `cond ? then : otherwise` (scalar or view branches).
    Ternary {
        cond: Box<TypedExpr>,
        then: Box<TypedExpr>,
        otherwise: Box<TypedExpr>,
    },
    /// A `count`/`sum`/`avg`/`min`/`max`/`distinct` aggregate (§7.5).
    Aggregate {
        func: AggFunc,
        source: Box<TypedExpr>,
        /// The aggregated field, or `None` for `count`.
        field: Option<String>,
    },
    /// A projection block over a view (§7.1, §7.2).
    Project {
        source: Box<TypedExpr>,
        projection: Projection,
    },
    /// A view union/intersection/difference (§7.4).
    Combine {
        op: CombineOp,
        lhs: Box<TypedExpr>,
        rhs: Box<TypedExpr>,
    },
    /// `view ?? other` — fallback when the first view is empty (§7.4).
    Fallback {
        primary: Box<TypedExpr>,
        other: Box<TypedExpr>,
    },
    /// The empty view `[]` (§7.4).
    EmptyView,
    /// A list literal in value position.
    List(Vec<TypedExpr>),
    /// A struct/object literal — named outputs (also a composite-key operand).
    Struct(Vec<(String, TypedExpr)>),
    /// A resolved built-in function call.
    Builtin {
        func: BuiltinFn,
        args: Vec<TypedExpr>,
    },
    /// `now()` — the fixed transaction sample (A.5).
    Now,
    /// `uuid()` — a generated UUID (§8.12); carries its call site so the
    /// environment can resolve per-call-site identity (SPEC-ISSUES item 4).
    Uuid,
}

/// A resolved row selector.
#[derive(Debug, Clone)]
pub(crate) enum TypedSelector {
    /// One or more independent key sources, concatenated in operand order
    /// (§6.3).
    Keys(Vec<TypedExpr>),
    /// A bound, optionally filtered selection `[:name | condition]` (§6.4).
    Bind {
        name: String,
        condition: Option<Box<TypedExpr>>,
    },
}

/// A resolved projection.
#[derive(Debug, Clone)]
pub(crate) struct Projection {
    /// The synthetic `$key` output field names, if grouping (§7.2).
    pub(crate) key: Vec<String>,
    /// The output fields, ordered so each depends only on earlier ones (§7.1).
    pub(crate) outputs: Vec<Output>,
    /// Sort keys, highest priority first (§7.3).
    pub(crate) sort: Vec<SortKey>,
    /// `$skip`, a non-negative row count (§7.3).
    pub(crate) skip: Option<u64>,
    /// `$limit`, a non-negative row count (§7.3).
    pub(crate) limit: Option<u64>,
}

/// One projected output field.
#[derive(Debug, Clone)]
pub(crate) struct Output {
    pub(crate) name: String,
    pub(crate) expr: TypedExpr,
}

/// One sort key (§7.3).
#[derive(Debug, Clone)]
pub(crate) struct SortKey {
    pub(crate) expr: TypedExpr,
    pub(crate) descending: bool,
}

/// The additive/negation numeric class of an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NumClass {
    Int,
    Decimal,
    /// `+` on `text` — string concatenation.
    TextConcat,
}

/// A resolved arithmetic operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// A resolved comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A resolved boolean connective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogicOp {
    And,
    Or,
}

/// A resolved aggregate function (§7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AggFunc {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    Distinct,
}

/// A resolved set-style view combinator (§7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CombineOp {
    Union,
    Intersect,
    Difference,
}

/// A resolved built-in function from the language surface or a namespace (§6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinFn {
    /// `size(x)` — text length in Unicode scalars, or collection/set cardinality.
    Size,
    /// `has(x)` — presence of an optional value or an import.
    Has,
    /// `assert(cond, message)` — admission condition (§8.8).
    Assert,
    /// `string.lower(text)`.
    StringLower,
    /// `string.upper(text)`.
    StringUpper,
    /// `string.trim(text)`.
    StringTrim,
}
