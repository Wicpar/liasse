//! The §15.6 read-facing accessors: `.<meter>.balance`, `.<meter>.pools`, and
//! `spend.funding`.
//!
//! These are folded onto the materialized row tree the same way computed values
//! are (§5.2): a row of a meter-declaring collection gains a `<meter>` cell whose
//! `balance` is the non-negative remainder of pool capacity after the allocations
//! held by extant spends, and a `$consumes` row gains a `funding` cell projecting
//! its frozen allocation. The context-free forms use the current instant and no
//! spend context (§15.6); the parameterized `balance({…})`/`pools({…})` call
//! forms remain a documented seam.

use std::collections::BTreeMap;

use bigdecimal::BigDecimal;
use liasse_expr::{Cell, Row, RowId};
use liasse_store::RowAddress;
use liasse_value::{Decimal, Text, Value};

use crate::eval::EvalCtx;
use crate::materialize;
use crate::state::Prospective;

use super::admit::funding_cell;
use super::resolve::{resolve_pools, SpendContext};
use super::CompiledMeter;

/// Fold every meter accessor and `funding` cell onto the package-root row tree
/// (§15.6). A no-op when the package declares no meters.
pub(crate) fn expose(ctx: &EvalCtx<'_>, prospective: &Prospective, root: Row) -> Row {
    let meters = &ctx.compiled.meters;
    if meters.meters.is_empty() && meters.spends.is_empty() {
        return root;
    }
    let mut extras: BTreeMap<RowId, Vec<(String, Cell)>> = BTreeMap::new();
    for address in prospective.working().keys() {
        let Some(id) = materialize::row_id_of(address) else { continue };
        let decl = decl_path(address);
        for meter in meters.meters.iter().filter(|m| m.path == decl) {
            let cell = meter_cell(ctx, prospective, meter, address);
            extras.entry(id.clone()).or_default().push((meter.name.clone(), cell));
        }
        if meters.spend_at(&decl).is_some()
            && let Some(fields) = prospective.get(address)
        {
            extras.entry(id).or_default().push(("funding".to_owned(), funding_cell(fields)));
        }
    }
    graft(root, &extras)
}

/// Augment a single materialized row (§15.6) with its meter/funding cells — the
/// return-value path for a `return spend { funding }` / enforcing-row receiver.
pub(crate) fn augment_row(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    decl: &[String],
    address: &RowAddress,
    row: Row,
) -> Row {
    let meters = &ctx.compiled.meters;
    let mut cells: Vec<(String, Cell)> = row.cells().map(|(n, c)| (n.clone(), c.clone())).collect();
    for meter in meters.meters.iter().filter(|m| m.path == decl) {
        set_cell(&mut cells, &meter.name, meter_cell(ctx, prospective, meter, address));
    }
    if meters.spend_at(decl).is_some()
        && let Some(fields) = prospective.get(address)
    {
        set_cell(&mut cells, "funding", funding_cell(fields));
    }
    Row::new(row.id().clone(), row.key().clone(), cells)
}

/// The `<meter>` accessor cell: `{ balance, pools }` (§15.6).
fn meter_cell(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    meter: &CompiledMeter,
    enforcing: &RowAddress,
) -> Cell {
    let context = SpendContext { cell: Cell::Row(Box::new(Row::keyless(RowId::leaf(0), []))), time: ctx.now() };
    let pools = resolve_pools(ctx, prospective, meter, enforcing, &context).unwrap_or_default();
    let level = meter.level_id(enforcing);
    let zero = BigDecimal::from(0);
    let mut balance = zero.clone();
    let mut pool_rows: Vec<Row> = Vec::new();
    for (index, pool) in pools.iter().enumerate() {
        let Some(quantity) = &pool.quantity else { continue };
        let remaining =
            quantity - super::admit::allocated(prospective, &level, &pool.source, &pool.key, &pool.incarnation);
        let remaining = if remaining < zero { zero.clone() } else { remaining };
        balance += remaining.clone();
        pool_rows.push(Row::new(
            RowId::leaf(index as u64),
            pool.key.clone(),
            [
                ("source".to_owned(), Cell::Scalar(Value::Text(Text::new(pool.source.clone())))),
                ("remaining".to_owned(), Cell::Scalar(Value::Decimal(Decimal::from_big_decimal(remaining)))),
            ],
        ));
    }
    Cell::Row(Box::new(Row::keyless(
        RowId::leaf(0),
        [
            ("balance".to_owned(), Cell::Scalar(Value::Decimal(Decimal::from_big_decimal(balance)))),
            ("pools".to_owned(), Cell::Collection(pool_rows)),
        ],
    )))
}

/// Graft the extra accessor cells onto every matching row of the tree by identity.
fn graft(row: Row, extras: &BTreeMap<RowId, Vec<(String, Cell)>>) -> Row {
    let mut cells: Vec<(String, Cell)> = row
        .cells()
        .map(|(name, cell)| (name.clone(), graft_cell(cell.clone(), extras)))
        .collect();
    if let Some(added) = extras.get(row.id()) {
        for (name, cell) in added {
            set_cell(&mut cells, name, cell.clone());
        }
    }
    Row::new(row.id().clone(), row.key().clone(), cells)
}

fn graft_cell(cell: Cell, extras: &BTreeMap<RowId, Vec<(String, Cell)>>) -> Cell {
    match cell {
        Cell::Collection(rows) => Cell::Collection(rows.into_iter().map(|r| graft(r, extras)).collect()),
        Cell::Row(row) => Cell::Row(Box::new(graft(*row, extras))),
        scalar => scalar,
    }
}

fn set_cell(cells: &mut Vec<(String, Cell)>, name: &str, cell: Cell) {
    match cells.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = cell,
        None => cells.push((name.to_owned(), cell)),
    }
}

fn decl_path(address: &RowAddress) -> Vec<String> {
    address.steps().map(|s| s.name().as_str().to_owned()).collect()
}
