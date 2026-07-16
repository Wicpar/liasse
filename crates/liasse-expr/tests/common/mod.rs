//! Shared test harness: a hand-built [`Scope`]/[`Environment`] pair and small
//! builders, so each test states exactly the state tree its expectation is
//! derived from.

// Test harness: failures surface as panics (AGENTS.md: tests are expected to
// panic on failed cases), which the workspace deny-lints otherwise forbid.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;

use liasse_diag::{Diagnostics, SourceMap};
use liasse_expr::{
    CallSite, Cell, Environment, EvalError, ExprType, Row, RowId, RowType, Scope, TemporalQuery,
    TypedExpr,
};
use liasse_syntax::parse_expression;
use liasse_value::{Decimal, Integer, Text, Timestamp, Type, Uuid, Value};

/// A scope with an explicit lexical current chain plus out-of-band maps.
pub struct FixedScope {
    pub contexts: Vec<ExprType>,
    pub root: Option<ExprType>,
    pub params: BTreeMap<String, ExprType>,
    pub structurals: BTreeMap<String, ExprType>,
    pub imports: BTreeMap<String, ExprType>,
    pub bindings: BTreeMap<String, ExprType>,
}

impl FixedScope {
    pub fn new(current: ExprType) -> Self {
        Self {
            root: Some(current.clone()),
            contexts: vec![current],
            params: BTreeMap::new(),
            structurals: BTreeMap::new(),
            imports: BTreeMap::new(),
            bindings: BTreeMap::new(),
        }
    }

    pub fn with_contexts(contexts: Vec<ExprType>, root: ExprType) -> Self {
        Self {
            root: Some(root),
            contexts,
            params: BTreeMap::new(),
            structurals: BTreeMap::new(),
            imports: BTreeMap::new(),
            bindings: BTreeMap::new(),
        }
    }

    pub fn param(mut self, name: &str, ty: ExprType) -> Self {
        self.params.insert(name.to_owned(), ty);
        self
    }

    pub fn structural(mut self, name: &str, ty: ExprType) -> Self {
        self.structurals.insert(name.to_owned(), ty);
        self
    }
}

impl Scope for FixedScope {
    fn current(&self) -> Option<ExprType> {
        self.contexts.last().cloned()
    }
    fn parent(&self, depth: u32) -> Option<ExprType> {
        self.contexts
            .len()
            .checked_sub(1 + depth as usize)
            .and_then(|idx| self.contexts.get(idx))
            .cloned()
    }
    fn root(&self) -> Option<ExprType> {
        self.root.clone()
    }
    fn param(&self, name: &str) -> Option<ExprType> {
        self.params.get(name).cloned()
    }
    fn structural(&self, name: &str) -> Option<ExprType> {
        self.structurals.get(name).cloned()
    }
    fn import(&self, name: &str) -> Option<ExprType> {
        self.imports.get(name).cloned()
    }
    fn binding(&self, name: &str) -> Option<ExprType> {
        self.bindings.get(name).cloned()
    }
}

/// A deterministic environment: a root row, out-of-band maps, and fixed `now`
/// / `uuid` samples so purity is testable.
pub struct FixedEnv {
    pub root: Row,
    pub params: BTreeMap<String, Cell>,
    pub structurals: BTreeMap<String, Cell>,
    pub now: Timestamp,
    pub uuid: Uuid,
}

impl FixedEnv {
    pub fn new(root: Row) -> Self {
        Self {
            root,
            params: BTreeMap::new(),
            structurals: BTreeMap::new(),
            now: Timestamp::new(1_700_000_000_000_000, liasse_value::Precision::Micros),
            uuid: Uuid::from_bytes([7; 16]),
        }
    }

    pub fn param(mut self, name: &str, cell: Cell) -> Self {
        self.params.insert(name.to_owned(), cell);
        self
    }

    pub fn structural(mut self, name: &str, cell: Cell) -> Self {
        self.structurals.insert(name.to_owned(), cell);
        self
    }
}

impl Environment for FixedEnv {
    fn root(&self) -> &Row {
        &self.root
    }
    fn param(&self, name: &str) -> Option<Cell> {
        self.params.get(name).cloned()
    }
    fn structural(&self, name: &str) -> Option<Cell> {
        self.structurals.get(name).cloned()
    }
    fn import(&self, _name: &str) -> Option<Cell> {
        None
    }
    fn now(&self) -> Timestamp {
        self.now
    }
    fn uuid(&self, _site: CallSite) -> Uuid {
        self.uuid
    }

    /// A test temporal index (§14.1): each base row's `[from, until)` interval is
    /// read from its `from`/`until` timestamp cells (absent = unbounded), exactly
    /// the half-open activity rule of SPEC §14.1–§14.2.
    fn temporal(&self, base: &[Row], query: &TemporalQuery) -> Result<Vec<Row>, EvalError> {
        let kept = base
            .iter()
            .filter(|row| {
                let from = timestamp_cell(row, "from");
                let until = timestamp_cell(row, "until");
                match query {
                    TemporalQuery::All => true,
                    TemporalQuery::At(at) => {
                        from.is_none_or(|f| *at >= f) && until.is_none_or(|u| *at < u)
                    }
                    TemporalQuery::Between(a, b) => {
                        from.is_none_or(|f| f < *b) && until.is_none_or(|u| u > *a)
                    }
                }
            })
            .cloned()
            .collect();
        Ok(kept)
    }
}

/// A row's `timestamp` bound cell, or `None` when absent/`none` (unbounded).
fn timestamp_cell(row: &Row, name: &str) -> Option<Timestamp> {
    match row.cell(name)?.as_scalar()? {
        Value::Timestamp(instant) => Some(*instant),
        _ => None,
    }
}

/// Parse and type-check, panicking with the rendered diagnostics on rejection.
pub fn check(scope: &dyn Scope, source: &str) -> TypedExpr {
    let mut sources = SourceMap::new();
    let id = sources.add_label("test", source);
    let parsed = parse_expression(id, source)
        .unwrap_or_else(|diags| panic!("parse failed:\n{}", diags.render(&sources)));
    liasse_expr::check_statement(scope, id, &parsed)
        .unwrap_or_else(|diags| panic!("check failed:\n{}", diags.render(&sources)))
}

/// Parse and type-check, expecting rejection; returns the diagnostics.
pub fn check_rejects(scope: &dyn Scope, source: &str) -> Diagnostics {
    let mut sources = SourceMap::new();
    let id = sources.add_label("test", source);
    let parsed = parse_expression(id, source)
        .unwrap_or_else(|diags| panic!("parse failed:\n{}", diags.render(&sources)));
    match liasse_expr::check_statement(scope, id, &parsed) {
        Ok(_) => panic!("expected type error, but `{source}` checked"),
        Err(diags) => diags,
    }
}

/// Type-check then evaluate against `env` with `current` as `.`.
pub fn eval(scope: &dyn Scope, env: &dyn Environment, current: &Cell, source: &str) -> Cell {
    let typed = check(scope, source);
    typed
        .evaluate(env, current)
        .unwrap_or_else(|err| panic!("evaluation failed: {}", err.message()))
}

/// Type-check then evaluate, returning the raw result (for error assertions).
pub fn try_eval(
    scope: &dyn Scope,
    env: &dyn Environment,
    current: &Cell,
    source: &str,
) -> Result<Cell, liasse_expr::EvalError> {
    check(scope, source).evaluate(env, current)
}

/// The scalar value a cell holds, or a panic describing the actual shape.
pub fn as_scalar(cell: &Cell) -> Value {
    cell.as_scalar()
        .cloned()
        .unwrap_or_else(|| panic!("expected a scalar cell, got {cell:?}"))
}

/// A projected view's rows, each as its ordered field map (name → scalar).
pub fn rows_fields(cell: &Cell) -> Vec<Vec<(String, Value)>> {
    let rows = cell
        .as_collection()
        .unwrap_or_else(|| panic!("expected a collection, got {cell:?}"));
    rows.iter()
        .map(|row| {
            row.cells()
                .filter_map(|(name, cell)| {
                    cell.as_scalar().map(|value| (name.clone(), value.clone()))
                })
                .collect()
        })
        .collect()
}

/// The stable occurrence identity of every row in a collection, in order — the
/// key-derived [`RowId`] a view delta is diffed by (§12.4, Annex B.5).
pub fn row_ids(cell: &Cell) -> Vec<RowId> {
    cell.as_collection()
        .unwrap_or_else(|| panic!("expected a collection, got {cell:?}"))
        .iter()
        .map(|row| row.id().clone())
        .collect()
}

/// The `text` field of every row, in order — a compact identity check.
pub fn ids(cell: &Cell, field: &str) -> Vec<Value> {
    cell.as_collection()
        .unwrap_or_else(|| panic!("expected a collection, got {cell:?}"))
        .iter()
        .filter_map(|row| row.cell(field).and_then(Cell::as_scalar).cloned())
        .collect()
}

// ---- value builders -------------------------------------------------------

pub fn vint(value: i64) -> Value {
    Value::Int(Integer::from(value))
}
pub fn vbig(text: &str) -> Value {
    Value::Int(Integer::parse(text).expect("int literal"))
}
pub fn vdec(text: &str) -> Value {
    Value::Decimal(Decimal::parse(text).expect("decimal literal"))
}
pub fn vtext(text: &str) -> Value {
    Value::Text(Text::new(text))
}

pub fn scell(value: Value) -> Cell {
    Cell::Scalar(value)
}

// ---- type / row builders --------------------------------------------------

pub fn scalar(ty: Type) -> ExprType {
    ExprType::scalar(ty)
}
pub fn view(row: RowType) -> ExprType {
    ExprType::View(row)
}
pub fn rowt(row: RowType) -> ExprType {
    ExprType::Row(row)
}
pub fn row_type(fields: Vec<(&str, ExprType)>, key: Option<ExprType>) -> RowType {
    RowType::new(fields.into_iter().map(|(n, t)| (n.to_owned(), t)), key)
}

pub fn row(id: u64, key: Value, cells: Vec<(&str, Cell)>) -> Row {
    Row::new(
        RowId::leaf(id),
        key,
        cells.into_iter().map(|(n, c)| (n.to_owned(), c)),
    )
}
pub fn keyless_row(id: u64, cells: Vec<(&str, Cell)>) -> Row {
    Row::keyless(RowId::leaf(id), cells.into_iter().map(|(n, c)| (n.to_owned(), c)))
}
/// A row whose occurrence identity is its canonical key text (Annex D.2) — the
/// stable, key-derived identity a materialized source row carries (§12.4).
pub fn keyed_row(key_text: &str, key: Value, cells: Vec<(&str, Cell)>) -> Row {
    Row::new(
        RowId::keyed(key_text),
        key,
        cells.into_iter().map(|(n, c)| (n.to_owned(), c)),
    )
}
pub fn collection(rows: Vec<Row>) -> Cell {
    Cell::Collection(rows)
}
