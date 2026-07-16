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

/// The identity of one row occurrence within an evaluation, ordered so it can
/// serve as the final sort tiebreaker (Annex B.5) and let a view result be
/// diffed by downstream crates.
///
/// It is a path of unsigned segments: a top-level row is `[k]`; a row reached
/// by descending into a nested collection extends its parent's path. Ordering
/// is lexicographic over the path, matching the source-row chain order a view
/// inherits (§7.2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(Vec<u64>);

impl RowId {
    /// A top-level occurrence identity.
    #[must_use]
    pub fn leaf(segment: u64) -> Self {
        Self(vec![segment])
    }

    /// Build from an explicit segment path.
    #[must_use]
    pub fn from_path(path: impl IntoIterator<Item = u64>) -> Self {
        Self(path.into_iter().collect())
    }

    /// The identity of a child occurrence one level deeper.
    #[must_use]
    pub fn child(&self, segment: u64) -> Self {
        let mut path = self.0.clone();
        path.push(segment);
        Self(path)
    }

    /// The path segments.
    #[must_use]
    pub fn segments(&self) -> &[u64] {
        &self.0
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
}
