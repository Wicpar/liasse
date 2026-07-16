//! Pool resolution shared by admission ([`admit`](super::admit)) and the §15.6
//! accessors ([`accessor`](super::accessor)).
//!
//! Given a meter, a concrete enforcing row, and a spend context (or the
//! context-free current instant), this resolves the ordered, eligible pools with
//! their capacity, evaluating each `$sources` view in the temporal context of the
//! spend (§15.1), coalescing repeated pool identities (§15.2 agree-or-reject),
//! gating by `$eligible`, and sorting by `$order` then pool identity.

use std::collections::BTreeMap;

use bigdecimal::BigDecimal;
use liasse_expr::{Cell, Row, RowId};
use liasse_value::{Decimal, Timestamp, Value};

use crate::error::{Rejection, RejectionReason};
use crate::eval::EvalCtx;
use crate::state::Prospective;

use super::{CompiledMeter, CompiledSource};

/// One resolved capacity pool (§15.2): its funding identity, capacity, active
/// interval, and the comparison keys `$order` produced.
pub(crate) struct Pool {
    pub(crate) source: String,
    pub(crate) key: Value,
    /// `None` for a partitioning (quantity-less) source — it never limits (§14.8).
    pub(crate) quantity: Option<BigDecimal>,
    pub(crate) order_keys: Vec<Value>,
}

/// The spend context a pool resolution is evaluated against (§15.2): the `spend`
/// binding cell for `$eligible` and the spend `$time` the sources see.
pub(crate) struct SpendContext {
    pub(crate) cell: Cell,
    pub(crate) time: Timestamp,
}

/// Resolve the ordered, eligible pools of `meter` for the enforcing row at
/// `enforcing_address`, evaluated at `context.time`. Duplicate pool identities
/// are coalesced (agree-or-reject, §15.2).
pub(crate) fn resolve_pools(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    meter: &CompiledMeter,
    enforcing_address: &liasse_store::RowAddress,
    context: &SpendContext,
) -> Result<Vec<Pool>, Rejection> {
    let Some(enforcing) =
        ctx.materialize_row_at(prospective, &meter.path, enforcing_address, context.time)
    else {
        return Ok(Vec::new());
    };
    let mut intervals = interval_index(&enforcing);
    // §15.1: a pool drawn from a source-backed bucket (§14.4) lives in a root
    // collection, not the enforcing row's subtree, so its `[from, until)` interval
    // comes from the derived-row index at the spend instant. The projected pool row
    // keeps the derived row's identity, so the same key resolves its interval.
    intervals.extend(ctx.source_bucket_interval_index(prospective, context.time));
    let current = Cell::Row(Box::new(enforcing.clone()));

    let mut pools: Vec<Pool> = Vec::new();
    let mut seen: BTreeMap<(String, Value), (Option<BigDecimal>, Option<Timestamp>)> = BTreeMap::new();
    for source in &meter.sources {
        for row in source_rows(ctx, prospective, source, &current, context.time)? {
            let interval = intervals.get(row.id()).copied().unwrap_or((None, None));
            // §15.1: a pool contributes only when active at the spend `$time`.
            if !active_at(interval, context.time) {
                continue;
            }
            let quantity = if source.has_quantity { pool_quantity(&row)? } else { None };
            let pool_row = with_pool_cells(&row, interval, quantity.as_ref());
            if !eligible(ctx, prospective, meter, &pool_row, context)? {
                continue;
            }
            let key = row.key().clone();
            let identity = (source.label.clone(), key.clone());
            // §15.2: a repeated full identity contributes one pool; a disagreement
            // rejects.
            if let Some((prev_qty, prev_until)) = seen.get(&identity) {
                if *prev_qty != quantity || *prev_until != interval.1 {
                    return Err(Rejection::new(
                        RejectionReason::Evaluation,
                        format!("meter `{}` pool identity repeats with disagreeing capacity", meter.name),
                    ));
                }
                continue;
            }
            seen.insert(identity, (quantity.clone(), interval.1));
            let order_keys = order_keys(ctx, prospective, meter, &pool_row, context.time)?;
            pools.push(Pool { source: source.label.clone(), key, quantity, order_keys });
        }
    }
    sort_pools(&mut pools, &meter.order);
    Ok(pools)
}

/// Evaluate one `$sources` view over the enforcing row at `time`.
fn source_rows(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    source: &CompiledSource,
    current: &Cell,
    time: Timestamp,
) -> Result<Vec<Row>, Rejection> {
    let env = ctx.env_at(prospective, time);
    match source.view.evaluate(&env, current).map_err(Rejection::from)? {
        Cell::Collection(rows) => Ok(rows),
        Cell::Row(row) => Ok(vec![*row]),
        Cell::Scalar(_) => Ok(Vec::new()),
    }
}

/// Evaluate `$eligible` (§15.2) for one pool. Absent `$eligible` admits every
/// pool.
fn eligible(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    meter: &CompiledMeter,
    pool_row: &Row,
    context: &SpendContext,
) -> Result<bool, Rejection> {
    let Some(eligible) = &meter.eligible else { return Ok(true) };
    let mut bindings = BTreeMap::new();
    bindings.insert("pool".to_owned(), Cell::Row(Box::new(pool_row.clone())));
    bindings.insert("spend".to_owned(), context.cell.clone());
    let env = ctx.env_at_full(prospective, context.time, bindings, BTreeMap::new());
    match eligible.evaluate(&env, &context.cell).map_err(Rejection::from)? {
        Cell::Scalar(Value::Bool(value)) => Ok(value),
        _ => Ok(false),
    }
}

/// Evaluate a pool's `$order` comparison keys (§15.2).
fn order_keys(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    meter: &CompiledMeter,
    pool_row: &Row,
    time: Timestamp,
) -> Result<Vec<Value>, Rejection> {
    if meter.order.is_empty() {
        return Ok(Vec::new());
    }
    let mut structurals = BTreeMap::new();
    for name in ["from", "until", "quantity"] {
        let cell = pool_row.cell(&format!("${name}")).cloned().unwrap_or(Cell::Scalar(Value::None));
        structurals.insert(name.to_owned(), cell);
    }
    let env = ctx.env_at_full(prospective, time, BTreeMap::new(), structurals);
    let current = Cell::Row(Box::new(pool_row.clone()));
    let mut keys = Vec::with_capacity(meter.order.len());
    for key in &meter.order {
        let value = match key.expr.evaluate(&env, &current).map_err(Rejection::from)? {
            Cell::Scalar(value) => value,
            _ => Value::None,
        };
        keys.push(value);
    }
    Ok(keys)
}

/// Sort pools by `$order` (each key's declared direction, none-last per B.2) with
/// pool identity as the final deterministic tiebreak (§15.2).
fn sort_pools(pools: &mut [Pool], order: &[super::OrderKey]) {
    pools.sort_by(|a, b| {
        for (index, key) in order.iter().enumerate() {
            let (Some(left), Some(right)) = (a.order_keys.get(index), b.order_keys.get(index)) else {
                continue;
            };
            let ordering = if key.descending { right.cmp(left) } else { left.cmp(right) };
            if ordering != std::cmp::Ordering::Equal {
                return ordering;
            }
        }
        (a.source.as_str(), &a.key).cmp(&(b.source.as_str(), &b.key))
    });
}

/// The `$quantity` capacity cell of a pool row as an exact decimal (§15.1),
/// rejecting a negative projected capacity is left to the caller.
fn pool_quantity(row: &Row) -> Result<Option<BigDecimal>, Rejection> {
    match row.cell("$quantity").and_then(Cell::as_scalar) {
        Some(value) => Ok(Some(decimal_of(value)?)),
        None => Ok(None),
    }
}

/// Read an exact decimal from a numeric value (`int`/`decimal`).
pub(crate) fn decimal_of(value: &Value) -> Result<BigDecimal, Rejection> {
    match value {
        Value::Decimal(d) => Ok(d.as_big_decimal().clone()),
        Value::Int(i) => Ok(BigDecimal::from(i.as_bigint().clone())),
        _ => Err(Rejection::new(RejectionReason::TypeError, "meter quantity is not a number")),
    }
}

/// A pool row augmented with its `$quantity`/`$from`/`$until` structural cells so
/// `$eligible` and `$order` read them.
fn with_pool_cells(row: &Row, (from, until): (Option<Timestamp>, Option<Timestamp>), quantity: Option<&BigDecimal>) -> Row {
    let mut cells: Vec<(String, Cell)> =
        row.cells().map(|(name, cell)| (name.clone(), cell.clone())).collect();
    set_cell(&mut cells, "$from", Cell::Scalar(from.map_or(Value::None, Value::Timestamp)));
    set_cell(&mut cells, "$until", Cell::Scalar(until.map_or(Value::None, Value::Timestamp)));
    if let Some(quantity) = quantity {
        set_cell(&mut cells, "$quantity", Cell::Scalar(Value::Decimal(Decimal::from_big_decimal(quantity.clone()))));
    }
    Row::new(row.id().clone(), row.key().clone(), cells)
}

fn set_cell(cells: &mut Vec<(String, Cell)>, name: &str, cell: Cell) {
    match cells.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = cell,
        None => cells.push((name.to_owned(), cell)),
    }
}

/// Whether `interval` is active at `time` — the half-open `[from, until)` rule
/// (§14.1).
fn active_at((from, until): (Option<Timestamp>, Option<Timestamp>), time: Timestamp) -> bool {
    from.is_none_or(|f| time >= f) && until.is_none_or(|u| time < u)
}

/// An index from row identity to its `[from, until)` interval, gathered from the
/// enforcing row's subtree (a bucketed pool row carries `$from`/`$until` cells).
fn interval_index(root: &Row) -> BTreeMap<RowId, (Option<Timestamp>, Option<Timestamp>)> {
    let mut index = BTreeMap::new();
    collect_intervals(root, &mut index);
    index
}

fn collect_intervals(row: &Row, index: &mut BTreeMap<RowId, (Option<Timestamp>, Option<Timestamp>)>) {
    let from = bound(row, "$from");
    let until = bound(row, "$until");
    if from.is_some() || until.is_some() {
        index.insert(row.id().clone(), (from, until));
    }
    for (_, cell) in row.cells() {
        match cell {
            Cell::Collection(rows) => {
                for nested in rows {
                    collect_intervals(nested, index);
                }
            }
            Cell::Row(nested) => collect_intervals(nested, index),
            Cell::Scalar(_) => {}
        }
    }
}

fn bound(row: &Row, name: &str) -> Option<Timestamp> {
    match row.cell(name).and_then(Cell::as_scalar) {
        Some(Value::Timestamp(instant)) => Some(*instant),
        _ => None,
    }
}
