//! The evaluation environment: the value tree an expression reads, plus the
//! generative and out-of-band bindings the runtime supplies.
//!
//! Evaluation is pure (SPEC.md §8.12): a [`TypedExpr`](crate::TypedExpr) is a
//! function of the [`Environment`] it is given and nothing else. The
//! environment — not the evaluator — owns every source of non-determinism, so
//! that "same environment ⇒ same result" holds by construction:
//!
//! - the logical state tree, as a package-root [`Row`];
//! - `@param`, `$structural`, and `#import` bindings;
//! - the fixed transaction-start `now()` sample (A.5) and `uuid()` values.
//!
//! # Generativity seam (SPEC-ISSUES item 4)
//!
//! §8.12 fixes `now()` for the whole admitted request and pins `uuid()`
//! per-request but *not* per call-site (SPEC-ISSUES item 4: two `uuid()` key
//! defaults in one request might collide). The evaluator therefore never
//! invents a UUID; it asks [`Environment::uuid`] with the [`CallSite`] of the
//! call. An environment MAY key its UUIDs on the call site (distinct values) or
//! ignore it (one value per request) — the choice is the runtime's, kept out of
//! the pure evaluator.

use std::collections::BTreeMap;

use liasse_diag::ByteSpan;
use liasse_value::{Timestamp, Uuid, Value};

use crate::error::EvalError;

/// One segment of a [`RowId`]: an identity component along the source-row chain
/// (§7.2, Annex D.1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RowIdPart {
    /// Identity derived from a keyed row's key: its canonical key text (Annex
    /// D.2). A source-collection row uses its collection key; a synthetic-`$key`
    /// group uses its group key rendered canonically. Stable across sibling
    /// insertions and deletions — the property a positional index lacks and the
    /// §12.4 view delta depends on.
    Key(String),
    /// An occurrence-order component: the Annex B.5 final tiebreaker, used when a
    /// row has no key identity (a keyless projection or scope root).
    Occurrence(u64),
}

/// The stable identity of one row occurrence within an evaluation, ordered so it
/// can serve as the final sort tiebreaker (Annex B.5) and let a view result be
/// diffed by identity across frontiers (§12.4).
///
/// It is a path of [`RowIdPart`]s along the source-row chain: a top-level row is
/// one segment; a row reached by descending into a nested collection extends its
/// parent's path (§7.2, Annex D.1). Ordering is lexicographic over the path.
/// Identity derives from the row's *key*, never its materialized position, so a
/// row keeps its identity when earlier rows disappear.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(Vec<RowIdPart>);

impl RowId {
    /// A top-level occurrence identity (the B.5 tiebreak for a keyless row).
    #[must_use]
    pub fn leaf(segment: u64) -> Self {
        Self(vec![RowIdPart::Occurrence(segment)])
    }

    /// A top-level key-derived identity: the row's canonical key text (D.2).
    #[must_use]
    pub fn keyed(text: impl Into<String>) -> Self {
        Self(vec![RowIdPart::Key(text.into())])
    }

    /// The identity of a child occurrence one level deeper (keyless tiebreak).
    #[must_use]
    pub fn child(&self, segment: u64) -> Self {
        self.extend(RowIdPart::Occurrence(segment))
    }

    /// The identity of a keyed child one level deeper (its canonical key text).
    #[must_use]
    pub fn child_keyed(&self, text: impl Into<String>) -> Self {
        self.extend(RowIdPart::Key(text.into()))
    }

    /// The identity components along the source-row chain.
    #[must_use]
    pub fn parts(&self) -> &[RowIdPart] {
        &self.0
    }

    fn extend(&self, part: RowIdPart) -> Self {
        let mut path = self.0.clone();
        path.push(part);
        Self(path)
    }
}

/// A member of a [`Row`]: a scalar value, a nested single row (a static struct
/// or a resolved single target row), or a collection of rows.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    /// A scalar or structured [`Value`] (text, int, ref, set, struct, `none`, …).
    Scalar(Value),
    /// A nested static struct or a single row.
    Row(Box<Row>),
    /// A keyed collection, set-as-rows, or view source, in canonical row order.
    Collection(Vec<Row>),
}

impl Cell {
    /// Wrap a scalar value.
    #[must_use]
    pub fn scalar(value: Value) -> Self {
        Self::Scalar(value)
    }

    /// The scalar value, if this cell is one.
    #[must_use]
    pub fn as_scalar(&self) -> Option<&Value> {
        match self {
            Self::Scalar(value) => Some(value),
            _ => None,
        }
    }

    /// The nested row, if this cell is one.
    #[must_use]
    pub fn as_row(&self) -> Option<&Row> {
        match self {
            Self::Row(row) => Some(row),
            _ => None,
        }
    }

    /// The rows, if this cell is a collection.
    #[must_use]
    pub fn as_collection(&self) -> Option<&[Row]> {
        match self {
            Self::Collection(rows) => Some(rows),
            _ => None,
        }
    }
}

/// A logical row: an occurrence identity, a typed key, and named cells.
///
/// The key is `Value::None` for a keyless scope (the package root or a static
/// struct). Cells are held in field-name order — the canonical struct order
/// (A.7 / B.4) a projection inherits.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    id: RowId,
    key: Value,
    cells: BTreeMap<String, Cell>,
}

impl Row {
    /// Assemble a row from its identity, key, and cells.
    #[must_use]
    pub fn new(
        id: RowId,
        key: Value,
        cells: impl IntoIterator<Item = (String, Cell)>,
    ) -> Self {
        Self {
            id,
            key,
            cells: cells.into_iter().collect(),
        }
    }

    /// A keyless scope row (package root / static struct).
    #[must_use]
    pub fn keyless(id: RowId, cells: impl IntoIterator<Item = (String, Cell)>) -> Self {
        Self::new(id, Value::None, cells)
    }

    /// The occurrence identity.
    #[must_use]
    pub fn id(&self) -> &RowId {
        &self.id
    }

    /// The typed row key (`Value::None` when keyless).
    #[must_use]
    pub fn key(&self) -> &Value {
        &self.key
    }

    /// A named cell.
    #[must_use]
    pub fn cell(&self, name: &str) -> Option<&Cell> {
        self.cells.get(name)
    }

    /// The cells in canonical field-name order.
    pub fn cells(&self) -> impl Iterator<Item = (&String, &Cell)> {
        self.cells.iter()
    }
}

/// The byte range of a generative call (`uuid()` / `now()`), passed to the
/// environment so it MAY resolve per-call-site identity (SPEC-ISSUES item 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallSite(ByteSpan);

impl CallSite {
    /// The call's source span.
    #[must_use]
    pub fn new(span: ByteSpan) -> Self {
        Self(span)
    }

    /// The underlying span.
    #[must_use]
    pub fn span(self) -> ByteSpan {
        self.0
    }
}

/// A resolved temporal selector over a bucketed base view (§14.1), carrying the
/// instants the evaluator has already reduced from the selector's argument
/// expressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemporalQuery {
    /// `.$at(t)` — the rows active at instant `t` (§14.1).
    At(Timestamp),
    /// `.$between(a, b)` — the rows whose half-open interval intersects the
    /// non-empty range `[a, b)`. The evaluator checks `b > a` before building
    /// this, so a query of this form always carries a non-empty range (§14.1).
    Between(Timestamp, Timestamp),
    /// `.$all` — every extant row, independent of current activity (§14.2).
    All,
}

/// The read-only, deterministic evaluation context an expression runs against.
///
/// Every method is a pure lookup: two evaluations against an environment that
/// answers identically MUST produce identical results (§8.12). Path resolution
/// (`/`, `.field`, selectors) is performed by the evaluator over the [`Row`]
/// tree; the environment only supplies the roots, out-of-band bindings, and the
/// two generative samples.
pub trait Environment {
    /// The package root (`/`).
    fn root(&self) -> &Row;

    /// A mutation or view parameter `@name` (§6.2, §8.3), if bound.
    fn param(&self, name: &str) -> Option<Cell>;

    /// A structural runtime binding `$name` — `$actor`, `$session`, `$config`,
    /// … (§6.2), if bound in the current feature context.
    fn structural(&self, name: &str) -> Option<Cell>;

    /// An imported module or parent-surface binding `#name` (§6.2), if bound.
    fn import(&self, name: &str) -> Option<Cell>;

    /// A lexical local binding `name` from the enclosing declaration (§6.2).
    /// Row bindings introduced *within* the expression are resolved by the
    /// evaluator's own frames, never here.
    fn binding(&self, name: &str) -> Option<Cell> {
        let _ = name;
        None
    }

    /// The single transaction-start wall-clock sample (A.5). Every `now()` in
    /// one admitted operation observes this same instant.
    fn now(&self) -> Timestamp;

    /// A generated UUID for the `uuid()` call at `site` (§8.12, SPEC-ISSUES
    /// item 4). The environment decides whether call sites share a value.
    fn uuid(&self, site: CallSite) -> Uuid;

    /// Resolve a temporal selector (§14.1) over a bucketed base view. `base` is
    /// the evaluated base collection's rows; the environment returns the rows
    /// `query` selects, deriving each row's `[from, until)` interval from the
    /// temporal index it owns (a bucketed collection's `$from`/`$until`).
    ///
    /// Keeping the index in the environment is what preserves purity: the
    /// evaluator hands over the base rows and the reduced instants and never
    /// computes activity itself. The default has no temporal index and rejects,
    /// so only a bucket-aware environment (the runtime) answers a temporal
    /// selector; ordinary expressions never reach this method.
    fn temporal(&self, base: &[Row], query: &TemporalQuery) -> Result<Vec<Row>, EvalError> {
        let _ = (base, query);
        Err(EvalError::NoTemporalIndex)
    }
}
