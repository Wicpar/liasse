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
//! recursion bound for the structural walks. The one non-structural recursion —
//! the checker's projection-output dependency ordering
//! (`check::walk::order_outputs`) — recurses on output-name edges and visits
//! each output at most once, so its depth is bounded by the projection's output
//! count, independently of the syntax cap.

use liasse_diag::ByteSpan;
use liasse_value::Value;

use crate::env::KeyringSelector;
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

    /// Whether this expression is the literal `none`.
    ///
    /// The bare literal types as the widest optional (`optional<json>`, A.7);
    /// per A.1 it is the absent value of *every* `optional<T>`, so the model
    /// layer — which owns assignment typing — narrows it against the target
    /// field's optional type. This query is what lets it recognise the literal.
    #[must_use]
    pub fn is_none_literal(&self) -> bool {
        matches!(self.kind, TypedKind::Literal(Value::None))
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
    /// A resolved host-namespace call `namespace.function(args)` (§16.2/§16.3):
    /// the local `$requires` namespace, the function, and the typed arguments,
    /// already checked against the pinned signature and the position's effect
    /// policy. The result type is this node's [`ExprType`](crate::ExprType);
    /// evaluation defers the call to the environment's host-call hook, which
    /// recomputes it purely (§16.3).
    HostCall {
        namespace: String,
        function: String,
        args: Vec<TypedExpr>,
    },
    /// `now()` — the fixed transaction sample (A.5).
    Now,
    /// `uuid()` — a generated UUID (§8.12); carries its call site so the
    /// environment can resolve per-call-site identity (SPEC-ISSUES item 4).
    Uuid,
    /// A temporal selector over a bucketed base view (§14.1): `.$at`,
    /// `.$between`, `.$all`. Evaluation reduces the query's instants and defers
    /// activity resolution to the environment's temporal index.
    Temporal {
        base: Box<TypedExpr>,
        query: TypedTemporal,
    },
    /// `base.$key` — the identity key value of a bound keyed row (§6.2, §6.3).
    /// The base is a single keyed row (`$actor`, a `login`/`session` binding, a
    /// keyed selection); the result is that row's canonical key value. Boxed so
    /// the node stays finite-sized.
    Key(Box<TypedExpr>),
    /// A keyring public version selector over a keyring's version view (§17.2):
    /// `.$current`, `.$accepted`, `.$public`, `.$versions`. Evaluation defers
    /// version-lifecycle resolution to the environment's keyring index; the
    /// resolved [`ExprType`](crate::ExprType) decides whether the result is the
    /// single active version (`.$current`, a row) or a version stream.
    Keyring {
        base: Box<TypedExpr>,
        selector: KeyringSelector,
    },
    /// A `blob` descriptor member selector (§18.1): `.$sha512`, `.$bytes`,
    /// `.$media`, `.$name`. The base is a `blob` value; evaluation reads the
    /// named member off its descriptor.
    BlobMember {
        base: Box<TypedExpr>,
        member: BlobMember,
    },
}

/// A `blob` descriptor member (§18.1). The complete descriptor is the
/// application value; these are its readable members.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlobMember {
    /// `$sha512` — the content hash, as its canonical lowercase-hex `text`.
    Sha512,
    /// `$bytes` — the non-negative `int` byte count.
    Bytes,
    /// `$media` — the canonical media type, as `text`.
    Media,
    /// `$name` — the optional file name (`optional<text>`).
    Name,
}

/// A resolved temporal selector form (§14.1). The instant operands are typed
/// `timestamp` expressions, reduced to values only at evaluation.
#[derive(Debug, Clone)]
pub(crate) enum TypedTemporal {
    /// `.$at(t)` — rows active at instant `t`.
    At(Box<TypedExpr>),
    /// `.$between(a, b)` — rows intersecting `[a, b)`.
    Between {
        start: Box<TypedExpr>,
        end: Box<TypedExpr>,
    },
    /// `.$all` — every extant row (§14.2).
    All,
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
    /// The `$quantity` pool-capacity expression, when this is a meter pool
    /// source view (§15.1). It assigns the structural `$quantity` role: an exact,
    /// non-negative `decimal` capacity that the runtime allocates against. Boxed
    /// so `Projection` (reached through `TypedKind::Project`) stays finite-sized.
    pub(crate) quantity: Option<Box<TypedExpr>>,
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
