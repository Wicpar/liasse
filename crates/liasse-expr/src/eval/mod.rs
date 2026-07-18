//! The evaluator: pure, deterministic evaluation of a [`TypedExpr`] against an
//! [`Environment`] (§6, §7, §8.12).
//!
//! Evaluation walks the typed tree — every operator/function/selector is
//! already resolved — and reads only the environment, so it is a pure function
//! of that environment. Row streams are carried as [`RowScope`]s (a row plus
//! the binding context a `[:name]` filter or `::` traversal introduces), so a
//! view result keeps its identity and order for downstream diffing.
//!
//! The recursion bound is the AST's 512-deep syntax cap (see [`crate::typed`]).

mod aggregate;
mod blob;
mod builtins;
mod decimal;
mod keyring;
mod ops;
mod temporal;
mod views;

use std::borrow::Cow;
use std::collections::BTreeMap;

use liasse_value::{RefKey, Text, Value};

use crate::env::{Cell, Environment, Row, RowId};
use crate::error::EvalError;
use crate::ty::ExprType;
use crate::typed::{TypedExpr, TypedKind, TypedSelector};

impl TypedExpr {
    /// Evaluate against `env` with `current` as the initial `.` (§6.2).
    pub fn evaluate(&self, env: &dyn Environment, current: &Cell) -> Result<Cell, EvalError> {
        self.evaluate_scoped(env, std::slice::from_ref(current))
    }

    /// Evaluate against `env` with an explicit lexical current chain: `currents`
    /// runs outermost-first, so its last element is `.` and earlier elements are
    /// reachable as `^`, `^^`, … (§6.2).
    pub fn evaluate_scoped(
        &self,
        env: &dyn Environment,
        currents: &[Cell],
    ) -> Result<Cell, EvalError> {
        self.evaluator(env, currents).eval(self)
    }

    /// Evaluate against `env` in *view context*: the result is delivered as a row
    /// view (a [`Cell::Collection`]), the shape a `$view` declaration returns
    /// (§12.2). This is the counterpart to [`Self::evaluate`] for the one place a
    /// selector's cardinality is fixed by the surrounding form rather than the
    /// expression: a `$view` is a stream, so a scalar/composite-key selection it
    /// wraps (`.people['a'] { … }`) yields its 0/1-row view — one row when the key
    /// exists, none when it is absent — never a coerced single row or the one-row
    /// cardinality rejection [`evaluate`](Self::evaluate) would raise (§6.3). A
    /// scalar or aggregate view result (`= size(.docs)`) passes through as itself.
    pub fn evaluate_view(&self, env: &dyn Environment, current: &Cell) -> Result<Cell, EvalError> {
        self.evaluate_view_scoped(env, std::slice::from_ref(current))
    }

    /// [`Self::evaluate_view`] with an explicit lexical current chain (§6.2).
    pub fn evaluate_view_scoped(
        &self,
        env: &dyn Environment,
        currents: &[Cell],
    ) -> Result<Cell, EvalError> {
        // §12.2: a scalar/aggregate view is one value, not a row stream; only a
        // row or view result is collected into the delivered collection.
        if matches!(self.ty(), ExprType::Scalar(_)) {
            return self.evaluate_scoped(env, currents);
        }
        self.evaluator(env, currents).collect_view(self)
    }

    fn evaluator<'a>(&self, env: &'a dyn Environment, currents: &[Cell]) -> Evaluator<'a> {
        Evaluator {
            env,
            frames: currents
                .iter()
                .map(|current| EvalFrame {
                    current: current.clone(),
                    bindings: BTreeMap::new(),
                })
                .collect(),
        }
    }
}

/// One evaluation frame: the current `.` and the bindings visible in it.
pub(crate) struct EvalFrame {
    pub(crate) current: Cell,
    pub(crate) bindings: BTreeMap<String, Cell>,
}

/// A row plus the binding context in which its projection is evaluated, and the
/// source-chain identity it contributes to a view output (§7.2, Annex D.1).
///
/// For an ordinary single-collection scope the identity is just the row's own
/// key-derived [`RowId`]; a `::` traversal level prepends the outer row's
/// identity, so `.modules::templates` inherits `modules.$key + templates.$key`
/// (§7.2/§13.9) and two instances exposing the same exposed key stay distinct.
pub(crate) struct RowScope {
    pub(crate) row: Row,
    pub(crate) binds: Vec<(String, Cell)>,
    pub(crate) identity: RowId,
}

impl RowScope {
    /// A bare scope over a single collection: its identity is the row's own,
    /// with no traversal prefix (§7.2).
    pub(crate) fn bare(row: Row) -> Self {
        let identity = row.id().clone();
        Self { row, binds: Vec::new(), identity }
    }
}

/// The evaluator state: the environment and the frame stack.
pub(crate) struct Evaluator<'a> {
    pub(crate) env: &'a dyn Environment,
    pub(crate) frames: Vec<EvalFrame>,
}

impl Evaluator<'_> {
    pub(crate) fn current_at(&self, depth: u32) -> Result<Cell, EvalError> {
        self.frames
            .len()
            .checked_sub(1 + depth as usize)
            .and_then(|idx| self.frames.get(idx))
            .map(|frame| frame.current.clone())
            .ok_or(EvalError::ShapeMismatch {
                expected: "a current value at this scope depth",
            })
    }

    fn binding(&self, name: &str) -> Option<Cell> {
        self.frames
            .iter()
            .rev()
            .find_map(|frame| frame.bindings.get(name).cloned())
    }

    pub(crate) fn push(&mut self, current: Cell) {
        self.frames.push(EvalFrame {
            current,
            bindings: BTreeMap::new(),
        });
    }

    pub(crate) fn pop(&mut self) {
        self.frames.pop();
    }

    pub(crate) fn bind(&mut self, name: String, value: Cell) {
        if let Some(frame) = self.frames.last_mut() {
            frame.bindings.insert(name, value);
        }
    }

    /// Evaluate one node to a [`Cell`].
    pub(crate) fn eval(&mut self, expr: &TypedExpr) -> Result<Cell, EvalError> {
        match expr.kind() {
            TypedKind::Literal(value) => Ok(Cell::Scalar(value.clone())),
            TypedKind::Root => Ok(Cell::Row(Box::new(self.env.root().clone()))),
            TypedKind::Current => self.current_at(0),
            TypedKind::Parent(depth) => self.current_at(*depth),
            TypedKind::Param(name) => self.out_of_band(self.env.param(name), "parameter", name),
            TypedKind::Structural(name) => match self.env.structural(name) {
                // A feature-context binding (`$actor`/`$session`/`$target`, §6.2).
                Some(cell) => Ok(cell),
                // §14.4: inside a projection over a source-backed bucket the checker
                // resolves `$index`/`$from`/`$until`/`$source` from the current row's
                // structural bindings; the derived row carries them as `$name` cells,
                // so read them off the current `.` when the environment has no binding.
                None => self
                    .row_structural(name)
                    .ok_or_else(|| EvalError::UnboundName { kind: "structural", name: name.clone() }),
            },
            TypedKind::Import(name) => self.out_of_band(self.env.import(name), "import", name),
            TypedKind::ScopeBinding(name) => {
                self.out_of_band(self.env.binding(name), "binding", name)
            }
            TypedKind::LocalBinding(name) => self.binding(name).ok_or_else(|| EvalError::UnboundName {
                kind: "binding",
                name: name.clone(),
            }),
            TypedKind::Field { base, name } => self.eval_field(base, name),
            TypedKind::Select { base, selector } => self.eval_select(expr, base, selector),
            TypedKind::Traverse { .. } => Ok(self.collect_view(expr)?),
            TypedKind::Arith { op, class, lhs, rhs } => self.eval_arith(*op, *class, lhs, rhs),
            TypedKind::Neg { class, operand } => self.eval_neg(*class, operand),
            TypedKind::Compare { op, lhs, rhs } => self.eval_compare(*op, lhs, rhs),
            TypedKind::Logic { op, lhs, rhs } => self.eval_logic(*op, lhs, rhs),
            TypedKind::Not(operand) => self.eval_not(operand),
            TypedKind::In { needle, haystack } => self.eval_in(needle, haystack),
            TypedKind::Ternary { cond, then, otherwise } => self.eval_ternary(cond, then, otherwise),
            TypedKind::Aggregate { func, source, field } => {
                self.eval_aggregate(expr, *func, source, field.as_deref())
            }
            TypedKind::Project { source, projection } => self.eval_project(expr, source, projection),
            TypedKind::Combine { op, lhs, rhs } => self.eval_combine(*op, lhs, rhs),
            TypedKind::Fallback { primary, other } => self.eval_fallback(expr, primary, other),
            TypedKind::EmptyView => Ok(Cell::Collection(Vec::new())),
            TypedKind::List(items) => self.eval_list(items),
            TypedKind::Struct(fields) => self.eval_struct(fields),
            TypedKind::Composite { order, source } => self.eval_composite(order, source),
            TypedKind::Builtin { func, args } => self.eval_builtin(*func, args),
            TypedKind::HostCall { namespace, function, args } => {
                self.eval_host_call(namespace, function, args)
            }
            TypedKind::Now => Ok(Cell::Scalar(Value::Timestamp(self.env.now()))),
            // §5.1/§8.12: the call site was pinned to its own sub-source at check
            // time, so two byte-identical `uuid()` defaults on one row carry
            // distinct sites and the environment derives distinct values.
            TypedKind::Uuid(site) => Ok(Cell::Scalar(Value::Uuid(self.env.uuid(*site)))),
            TypedKind::Key(base) => self.eval_key(base),
            TypedKind::Temporal { base, query } => self.eval_temporal(base, query),
            TypedKind::Keyring { base, selector } => self.eval_keyring(expr, base, *selector),
            TypedKind::BlobMember { base, member } => self.eval_blob_member(base, *member),
        }
    }

    /// The `$name` structural cell of the nearest enclosing `.` row (§14.4): a
    /// projection over a source-backed bucket reads the derived row's
    /// `$from`/`$until`/`$index`/`$source` cells. Mirrors the checker's
    /// `row_structural`, which types these off the current row's structural shape.
    fn row_structural(&self, name: &str) -> Option<Cell> {
        let cell_name = format!("${name}");
        self.frames.iter().rev().find_map(|frame| match &frame.current {
            Cell::Row(row) => row.cell(&cell_name).cloned(),
            _ => None,
        })
    }

    fn out_of_band(
        &self,
        cell: Option<Cell>,
        kind: &'static str,
        name: &str,
    ) -> Result<Cell, EvalError> {
        cell.ok_or_else(|| EvalError::UnboundName {
            kind,
            name: name.to_owned(),
        })
    }

    /// `base.$key` (§6.3): the identity key value of a bound keyed row. The
    /// checker has already proven the base is a keyed row, so evaluation reads the
    /// row's key value directly.
    fn eval_key(&mut self, base: &TypedExpr) -> Result<Cell, EvalError> {
        match self.eval(base)? {
            Cell::Row(row) => Ok(Cell::Scalar(row.key().clone())),
            _ => Err(EvalError::ShapeMismatch { expected: "a keyed row" }),
        }
    }

    fn eval_field(&mut self, base: &TypedExpr, name: &str) -> Result<Cell, EvalError> {
        let base = self.eval(base)?;
        match base {
            Cell::Row(row) => row
                .cell(name)
                .cloned()
                .ok_or(EvalError::ShapeMismatch { expected: "a row with this field" }),
            Cell::Scalar(Value::Struct(value)) => value
                .get(name)
                .cloned()
                .map(Cell::Scalar)
                .ok_or(EvalError::ShapeMismatch { expected: "a struct with this field" }),
            _ => Err(EvalError::ShapeMismatch { expected: "a row" }),
        }
    }

    fn eval_select(
        &mut self,
        expr: &TypedExpr,
        base: &TypedExpr,
        selector: &TypedSelector,
    ) -> Result<Cell, EvalError> {
        let rows = self.select_rows(base, selector)?;
        if matches!(expr.ty(), ExprType::Row(_)) {
            // §6.3: a lone scalar/composite key context requires exactly one row.
            return match rows.len() {
                1 => rows
                    .into_iter()
                    .next()
                    .map(|row| Cell::Row(Box::new(row)))
                    .ok_or(EvalError::ShapeMismatch { expected: "one row" }),
                found => Err(EvalError::Cardinality { context: "a scalar row selector", found }),
            };
        }
        Ok(Cell::Collection(rows))
    }

    /// Resolve a selector to its concatenated matching rows (§6.3).
    pub(crate) fn select_rows(
        &mut self,
        base: &TypedExpr,
        selector: &TypedSelector,
    ) -> Result<Vec<Row>, EvalError> {
        let source = self.eval(base)?;
        let rows = match source {
            Cell::Collection(rows) => rows,
            Cell::Row(row) => vec![*row],
            _ => return Err(EvalError::ShapeMismatch { expected: "a collection" }),
        };
        match selector {
            TypedSelector::Keys(keys) => self.select_by_keys(&rows, keys),
            TypedSelector::Bind { name, condition } => self.select_by_bind(rows, name, condition),
        }
    }

    pub(crate) fn select_by_keys(
        &mut self,
        rows: &[Row],
        keys: &[TypedExpr],
    ) -> Result<Vec<Row>, EvalError> {
        let mut selected = Vec::new();
        for key in keys {
            let value = self.eval(key)?;
            let wanted = match value {
                Cell::Scalar(value) => value,
                _ => return Err(EvalError::ShapeMismatch { expected: "a scalar key" }),
            };
            // §6.3: a set contributes keys in the target's canonical order; a
            // scalar/composite contributes zero or one row.
            match wanted {
                Value::Set(members) => {
                    for member in &members {
                        let wanted_key = ref_key_value(member);
                        selected.extend(
                            rows.iter().filter(|row| row.key() == wanted_key.as_ref()).cloned(),
                        );
                    }
                }
                scalar => {
                    let wanted_key = ref_key_value(&scalar);
                    selected.extend(
                        rows.iter().filter(|row| row.key() == wanted_key.as_ref()).cloned(),
                    );
                }
            }
        }
        Ok(selected)
    }

    fn select_by_bind(
        &mut self,
        rows: Vec<Row>,
        name: &str,
        condition: &Option<Box<TypedExpr>>,
    ) -> Result<Vec<Row>, EvalError> {
        let Some(condition) = condition else {
            return Ok(rows);
        };
        let mut kept = Vec::new();
        for row in rows {
            self.push(Cell::Row(Box::new(row.clone())));
            self.bind(name.to_owned(), Cell::Row(Box::new(row.clone())));
            let verdict = self.eval(condition);
            self.pop();
            if matches!(verdict?, Cell::Scalar(Value::Bool(true))) {
                kept.push(row);
            }
        }
        Ok(kept)
    }

    fn eval_list(&mut self, items: &[TypedExpr]) -> Result<Cell, EvalError> {
        let mut members = std::collections::BTreeSet::new();
        for item in items {
            match self.eval(item)? {
                Cell::Scalar(value) => {
                    members.insert(value);
                }
                _ => return Err(EvalError::ShapeMismatch { expected: "a scalar list element" }),
            }
        }
        Ok(Cell::Scalar(Value::Set(members)))
    }

    /// Normalize a composite key operand to `$key` order (A.9): evaluate `source`
    /// to a struct and pull each declared component (in `order`) into the
    /// positional [`Value::Composite`] tuple a composite row's key carries. An
    /// operand that already evaluated to a composite (e.g. another row's `.$key`)
    /// passes through.
    fn eval_composite(
        &mut self,
        order: &[String],
        source: &TypedExpr,
    ) -> Result<Cell, EvalError> {
        let value = self.eval_scalar(source)?;
        let components = match value {
            Value::Struct(fields) => order
                .iter()
                .map(|name| fields.get(name).cloned().unwrap_or(Value::None))
                .collect(),
            Value::Composite(components) => components,
            other => return Ok(Cell::Scalar(other)),
        };
        Ok(Cell::Scalar(Value::Composite(components)))
    }

    fn eval_struct(&mut self, fields: &[(String, TypedExpr)]) -> Result<Cell, EvalError> {
        let mut entries = Vec::with_capacity(fields.len());
        for (name, expr) in fields {
            match self.eval(expr)? {
                Cell::Scalar(value) => entries.push((Text::new(name.clone()), value)),
                _ => return Err(EvalError::ShapeMismatch { expected: "a scalar struct field" }),
            }
        }
        Ok(Cell::Scalar(Value::Struct(liasse_value::Struct::new(entries))))
    }
}

/// The comparable key value a selector operand denotes (§5.6, §6.3). A ref's
/// application-visible value is its target's current typed key: a scalar-keyed
/// ref compares as that inner scalar, and a composite-keyed ref as the positional
/// [`Value::Composite`] tuple of its components — the same value a composite
/// row's `key()` carries, so `.owner in .regions` / `.regions[.owner]` match by
/// value. Any non-ref value compares as itself.
fn ref_key_value(value: &Value) -> Cow<'_, Value> {
    match value {
        Value::Ref(reference) => match reference.key() {
            RefKey::Scalar(inner) => Cow::Borrowed(inner),
            RefKey::Composite(components) => Cow::Owned(Value::Composite(components.clone())),
        },
        other => Cow::Borrowed(other),
    }
}
