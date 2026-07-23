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
//! structurally on the AST. liasse-syntax caps expression *node* nesting depth at
//! `MAX_NESTING_DEPTH` before this crate ever sees a tree — the bracket prescan
//! bounds delimiter nesting, and a post-parse node-depth guard bounds bracket-free
//! chains (`!!!!x`, `.a.a.a`, `x+x+x`) the prescan cannot see — so that cap is the
//! recursion bound for the structural walks, and it is
//! calibrated to clear the checker's and evaluator's stack budget, which
//! overflows well below `pest`'s. The one non-structural recursion —
//! the checker's projection-output dependency ordering
//! (`check::walk::order_outputs`) — recurses on output-name edges and visits
//! each output at most once, so its depth is bounded by the projection's output
//! count, independently of the syntax cap.

use liasse_diag::ByteSpan;
use liasse_value::Value;

use crate::env::{CallSite, KeyringSelector};
use crate::ty::ExprType;

/// A type-checked expression: its span, its resolved result type, and its
/// resolved operation.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
pub struct TypedExpr {
    #[cfg_attr(feature = "eval-wire", serde(with = "crate::wire::byte_span_serde"))]
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

    /// The total order this view's result rows are delivered in (§7.3, §7.4): the
    /// per-key directions the outermost node fixes, resolving a reference to another
    /// view through `views`.
    ///
    /// A directly sorted projection fixes the order via its own `$sort` keys: it
    /// rebuilds fresh rows and either re-sorts (writing those keys) or preserves
    /// source order (leaving the tuple empty), so its own directions describe the
    /// delivered order exactly. Every other row-shaped node inherits its order from
    /// what it delivers, per §7.4:
    ///
    /// - a combinator (`a | b`, `a & b`, `a - b`) delivers in its **left** operand's
    ///   order — difference/intersection is a subset of the left, and union appends
    ///   new right identities after the whole left run — so it adopts `lhs`'s order;
    /// - a conditional (`cond ? a : b`) or fallback (`a ?? b`) delivers exactly one
    ///   branch's rows, so it takes the order the branches share (or the non-empty
    ///   sibling's, when one branch is the empty view), else occurrence identity;
    /// - a reference to a top-level named view (`.desc`) reads that view's rows off
    ///   the same-named cell folded onto the root row (§7.1), so it adopts the
    ///   referenced view's order, recovered through `views`;
    /// - a `[:name | condition]` filter or a `[key]` selection only narrows rows,
    ///   so it preserves its base view's order.
    ///
    /// Anything else (a bare data collection, a `::` traversal, a scalar) exposes no
    /// single `$sort` direction and is [`SortOrder::unordered`], so its rows fall
    /// back to occurrence-identity order (§8/Annex B.5). A bounded window partitions
    /// rows at its frozen gap coordinate through this order (§12.2), so returning the
    /// true delivered order here is what keeps that partition monotone — a
    /// combinator over a descending left view must report descending, not unordered,
    /// or the window's `partition_point` runs over a non-monotone slice and collapses.
    #[must_use]
    pub fn result_order(&self, views: &dyn crate::ViewOrders) -> crate::SortOrder {
        match &self.kind {
            TypedKind::Project { projection, .. } => crate::SortOrder::from_keys(&projection.sort),
            // §7.4: all three combinators pin the delivered order to the left operand.
            TypedKind::Combine { lhs, .. } => lhs.result_order(views),
            // §7.4: a conditional/fallback delivers one branch's rows.
            TypedKind::Ternary { then, otherwise, .. } => Self::branch_order(then, otherwise, views),
            TypedKind::Fallback { primary, other } => Self::branch_order(primary, other, views),
            // §7.1: `.desc` reads a top-level view's cell off the root row, so its
            // order is that view's. A field on any other receiver, or naming a bare
            // data collection, has no such order (occurrence identity).
            TypedKind::Field { base, name } if base.is_root_receiver() => {
                views.view_order(name).unwrap_or_default()
            }
            // §6.4/§7.3: a filter/selection narrows rows but keeps their order.
            TypedKind::Select { base, .. } => base.result_order(views),
            _ => crate::SortOrder::unordered(),
        }
    }

    /// The order two combinator/conditional branches jointly deliver (§7.4): the
    /// order they share, or the non-empty sibling's order when one branch is the
    /// empty view (`[]` contributes no rows to order), else occurrence identity —
    /// the rows come entirely from one branch, so a shared order is the delivered
    /// order and a disagreement leaves no single declared order.
    fn branch_order(a: &TypedExpr, b: &TypedExpr, views: &dyn crate::ViewOrders) -> crate::SortOrder {
        match (&a.kind, &b.kind) {
            (TypedKind::EmptyView, _) => b.result_order(views),
            (_, TypedKind::EmptyView) => a.result_order(views),
            _ => {
                let left = a.result_order(views);
                let right = b.result_order(views);
                if left == right { left } else { crate::SortOrder::unordered() }
            }
        }
    }

    /// Whether this node is the root receiver (`.` or `/`) a top-level view
    /// reference is read off (§7.1). A named `$view` is folded onto the root row, so
    /// only `.name`/`/name` — never a nested `row.name` — resolves to a view's order.
    fn is_root_receiver(&self) -> bool {
        matches!(self.kind, TypedKind::Current | TypedKind::Root)
    }
}

/// A resolved operation. Overloads (`+` as int/decimal/text, `-` as
/// arithmetic/difference, `==` on scalars vs refs) are already decided.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum TypedKind {
    /// A constant scalar (`1`, `"x"`, `true`, `none`, a `[]`/`{}` literal value).
    Literal(#[cfg_attr(feature = "eval-wire", serde(with = "crate::wire::value_serde"))] Value),
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
    /// A composite key operand normalized to `$key` order (A.9): the authoring
    /// object `{ name: … }` (or any struct-typed operand) reordered to the
    /// declared `$key` component order, so it evaluates to the positional
    /// [`Value::Composite`](liasse_value::Value::Composite) a composite row's key
    /// carries. `order` is the `$key` field names; `source` evaluates to the
    /// struct whose fields are pulled in that order.
    Composite {
        order: Vec<String>,
        source: Box<TypedExpr>,
    },
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
    /// `uuid()` — a generated UUID (§8.12); carries its globally unique
    /// [`CallSite`] (source + span) so the environment resolves a distinct value
    /// per call site, even for two byte-identical defaults compiled into
    /// different sub-sources (SPEC-ISSUES item 4, §5.1/§8.12).
    Uuid(#[cfg_attr(feature = "eval-wire", serde(with = "crate::wire::callsite_serde"))] CallSite),
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
    /// A `blob` descriptor or placement member selector (§18.1, §18.5):
    /// `.$sha512`, `.$bytes`, `.$media`, `.$name` read descriptor metadata, while
    /// `.$satisfied`, `.$stored`, `.$surplus` read the engine-recorded placement
    /// observations. The base is a `blob` value; a metadata member reads it off
    /// the descriptor directly, a placement member defers to the environment's
    /// placement index (as a temporal/keyring selector defers activity/lifecycle).
    BlobMember {
        base: Box<TypedExpr>,
        member: BlobMember,
    },
}

/// A `blob` descriptor member (§18.1) or logical placement member (§18.5). The
/// complete descriptor is the application value; the metadata members are read
/// off it, and the placement members are the engine's logical observations of
/// where the content is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum BlobMember {
    /// `$sha512` — the content hash, as its canonical lowercase-hex `text` (§18.1).
    Sha512,
    /// `$bytes` — the non-negative `int` byte count (§18.1).
    Bytes,
    /// `$media` — the canonical media type, as `text` (§18.1).
    Media,
    /// `$name` — the optional file name (`optional<text>`, §18.1).
    Name,
    /// `$satisfied` — whether the current placement policy is satisfied over the
    /// verified stores (`bool`, §18.5).
    Satisfied,
    /// `$stored` — the verified stores holding this content, as a view of keyed
    /// store-identity rows (§18.5).
    Stored,
    /// `$surplus` — the verified copies outside the currently required policy, as
    /// a view of keyed store-identity rows (§18.5).
    Surplus,
}

/// A resolved temporal selector form (§14.1). The instant operands are typed
/// `timestamp` expressions, reduced to values only at evaluation.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct Output {
    pub(crate) name: String,
    pub(crate) expr: TypedExpr,
}

/// One sort key (§7.3).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct SortKey {
    pub(crate) expr: TypedExpr,
    pub(crate) descending: bool,
}

/// The additive/negation numeric class of an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum NumClass {
    Int,
    Decimal,
    /// `+` on `text` — string concatenation.
    TextConcat,
    /// `timestamp ± duration` (or `duration + timestamp`) — a temporal shift
    /// yielding a `timestamp` (§11.5 `now() + time.duration('P30D')`).
    TimeShift,
}

/// A resolved arithmetic operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// A resolved comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum LogicOp {
    And,
    Or,
}

/// A resolved aggregate function (§7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum CombineOp {
    Union,
    Intersect,
    Difference,
}

/// A resolved built-in function from the language surface or a namespace (§6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
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
    /// `string.casefold(text)` — Unicode default (full, C+F) case fold (§6.5).
    StringCasefold,
    /// `string.trim(text)`.
    StringTrim,
    /// `time.duration(text)` — parse an ISO-8601 duration literal (§16.1).
    TimeDuration,
}
