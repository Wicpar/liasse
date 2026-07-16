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
mod builtins;
mod ops;
mod views;

use std::collections::BTreeMap;

use liasse_value::{Text, Value};

use crate::env::{Cell, Environment, Row};
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
        let mut evaluator = Evaluator {
            env,
            frames: currents
                .iter()
                .map(|current| EvalFrame {
                    current: current.clone(),
                    bindings: BTreeMap::new(),
                })
                .collect(),
        };
        evaluator.eval(self)
    }
}

/// One evaluation frame: the current `.` and the bindings visible in it.
pub(crate) struct EvalFrame {
    pub(crate) current: Cell,
    pub(crate) bindings: BTreeMap<String, Cell>,
}

/// A row plus the binding context in which its projection is evaluated.
pub(crate) struct RowScope {
    pub(crate) row: Row,
    pub(crate) binds: Vec<(String, Cell)>,
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
            TypedKind::Structural(name) => {
                self.out_of_band(self.env.structural(name), "structural", name)
            }
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
            TypedKind::Builtin { func, args } => self.eval_builtin(*func, args),
            TypedKind::Now => Ok(Cell::Scalar(Value::Timestamp(self.env.now()))),
            TypedKind::Uuid => Ok(Cell::Scalar(Value::Uuid(
                self.env.uuid(crate::env::CallSite::new(expr.span())),
            ))),
        }
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
    fn select_rows(
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

    fn select_by_keys(
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
                        selected.extend(rows.iter().filter(|row| row.key() == member).cloned());
                    }
                }
                scalar => {
                    selected.extend(rows.iter().filter(|row| row.key() == &scalar).cloned());
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
