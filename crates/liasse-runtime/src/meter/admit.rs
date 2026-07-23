//! Meter admission (§15.2): fund every new or changed spend and reject the whole
//! transition when eligible capacity is insufficient.
//!
//! The chosen allocation is frozen onto the spend row (the [`FUNDING_FIELD`]
//! structural field) as an admission fact, so a later pool change never rewrites
//! it (§15.2/§15.3). Deleting a spend drops the field and releases its capacity;
//! updating a spend clears and reallocates it. Balance is always the current pool
//! quantity minus the allocations held by the extant spend rows.

use bigdecimal::BigDecimal;
use liasse_expr::{Cell, Row, RowId};
use liasse_store::{AddressStep, RowAddress};
use liasse_value::{Decimal, Struct, Text, Timestamp, Value};

use crate::error::{Rejection, RejectionReason};
use crate::eval::EvalCtx;
use crate::materialize::FieldMap;
use crate::state::Prospective;

use super::resolve::{decimal_of, resolve_pools, SpendContext};
use super::{CompiledMeter, CompiledMeters, CompiledSpend, SpendConsume, FUNDING_FIELD};

/// One frozen funding allocation (§15.3): the enforcing level it clears, the
/// source and pool identity it drew from, and the exact amount allocated.
struct Entry {
    level: String,
    source: String,
    pool: Value,
    amount: BigDecimal,
}

/// Fund every new or changed spend among `touched`, rejecting the whole
/// transition on insufficient or zero eligible capacity (§15.2). A no-op when the
/// package declares no meters.
pub(crate) fn enforce(
    ctx: &EvalCtx<'_>,
    meters: &CompiledMeters,
    prospective: &mut Prospective,
    touched: &[RowAddress],
) -> Result<(), Rejection> {
    // §15.1: every projected pool `$quantity` in committed state MUST be
    // non-negative. Check it eagerly, at admission of the producing transition,
    // before any spend funding — a pool source insert/edit that drives a projected
    // capacity below zero is rejected here, never deferred to a later meter read.
    // Runs even when the package declares no spend (pools without consumers still
    // hold the invariant).
    enforce_pool_quantities(ctx, meters, prospective)?;
    if meters.spends.is_empty() {
        return Ok(());
    }
    for address in touched {
        let decl = decl_path(address);
        let Some(spend) = meters.spend_at(&decl) else { continue };
        fund_spend(ctx, meters, prospective, address, &decl, spend)?;
    }
    Ok(())
}

/// §15.1: reject any transition whose committed state would project a negative
/// pool `$quantity`. Every limiting meter's live enforcing rows are re-projected
/// and each projected capacity is checked, so a pool-source write anywhere —
/// including a root-collection-derived pool that lives outside the enforcing row's
/// subtree — is caught at admission. Because committed state already satisfies the
/// invariant inductively, this is the only new surface an admission can introduce.
fn enforce_pool_quantities(
    ctx: &EvalCtx<'_>,
    meters: &CompiledMeters,
    prospective: &Prospective,
) -> Result<(), Rejection> {
    for meter in &meters.meters {
        if !meter.limiting {
            continue;
        }
        for enforcing in enforcing_addresses(prospective, &meter.path) {
            super::resolve::check_pool_quantities(ctx, prospective, meter, &enforcing)?;
        }
    }
    Ok(())
}

/// The addresses of the live enforcing rows of a meter declared at `path`: every
/// working row whose declaration-name path equals `path` (§15.4). One meter
/// declaration enforces at each such row instance.
fn enforcing_addresses(prospective: &Prospective, path: &[String]) -> Vec<RowAddress> {
    prospective
        .working()
        .keys()
        .filter(|address| decl_path(address) == path)
        .cloned()
        .collect()
}

/// Fund one spend row: clear any prior allocation, evaluate each consumed meter's
/// amount/time/metadata, drain the eligible pools at every enforcing level, and
/// freeze the result onto the row (§15.2).
fn fund_spend(
    ctx: &EvalCtx<'_>,
    meters: &CompiledMeters,
    prospective: &mut Prospective,
    address: &RowAddress,
    decl: &[String],
    spend: &CompiledSpend,
) -> Result<(), Rejection> {
    // §15.2: provisionally release the spend's current allocation before
    // reallocating, so it never funds against its own held capacity.
    let mut fields = prospective.get(address).cloned().unwrap_or_default();
    fields.remove(FUNDING_FIELD);
    prospective.replace(address, fields.clone());

    let now = ctx.now();
    let Some(spend_row) = ctx.materialize_row_at(prospective, decl, address, now) else {
        return Ok(());
    };
    let spend_cell = Cell::Row(Box::new(spend_row.clone()));

    let mut entries: Vec<Entry> = Vec::new();
    for consume in &spend.consumes {
        let amount = eval_decimal(ctx, prospective, &consume.amount, &spend_cell, now)?;
        if amount < zero() {
            return Err(Rejection::new(
                RejectionReason::Evaluation,
                format!("spend $amount MUST be non-negative for meter `{}`", consume.meter),
            )
            .at(address.render()));
        }
        let time = eval_time(ctx, prospective, &consume.time, &spend_cell, now)?;
        let cell = spend_context_cell(ctx, prospective, &spend_row, consume, &amount, time, now)?;
        let context = SpendContext { cell, time };
        fund_consume(ctx, meters, prospective, address, decl, consume, &amount, &context, &mut entries)?;
    }

    fields.insert(FUNDING_FIELD.to_owned(), encode(&entries));
    prospective.replace(address, fields);
    Ok(())
}

/// Fund one consumed meter at every reachable enforcing level (§15.4). A zero
/// amount produces no funding; a purely partitioning meter never limits (§14.8).
#[allow(clippy::too_many_arguments)]
fn fund_consume(
    ctx: &EvalCtx<'_>,
    meters: &CompiledMeters,
    prospective: &Prospective,
    address: &RowAddress,
    decl: &[String],
    consume: &SpendConsume,
    amount: &BigDecimal,
    context: &SpendContext,
    entries: &mut Vec<Entry>,
) -> Result<(), Rejection> {
    for (meter, enforcing) in reachable_levels(meters, address, decl, &consume.meter) {
        if !meter.limiting || *amount == zero() {
            continue;
        }
        let pools = resolve_pools(ctx, prospective, meter, &enforcing, context)?;
        let level = meter.level_id(&enforcing);
        drain(prospective, meter, &level, &pools, amount, entries)?;
    }
    Ok(())
}

/// Drain `amount` across `pools` in order, appending the allocation to `entries`,
/// rejecting when the eligible remainder is insufficient (§15.2).
fn drain(
    prospective: &Prospective,
    meter: &CompiledMeter,
    level: &str,
    pools: &[super::resolve::Pool],
    amount: &BigDecimal,
    entries: &mut Vec<Entry>,
) -> Result<(), Rejection> {
    let mut needed = amount.clone();
    let mut planned: Vec<Entry> = Vec::new();
    for pool in pools {
        let Some(quantity) = &pool.quantity else { continue };
        let mut held = allocated(prospective, level, &pool.source, &pool.key);
        for entry in &planned {
            if entry.source == pool.source && entry.pool == pool.key {
                held += entry.amount.clone();
            }
        }
        let available = quantity - held;
        if available <= zero() {
            continue;
        }
        let take = if available < needed { available } else { needed.clone() };
        planned.push(Entry { level: level.to_owned(), source: pool.source.clone(), pool: pool.key.clone(), amount: take.clone() });
        needed -= take;
        if needed == zero() {
            break;
        }
    }
    if needed > zero() {
        return Err(Rejection::new(
            RejectionReason::Evaluation,
            format!("meter `{}` lacks eligible capacity for the spend (§15.2)", meter.name),
        ));
    }
    entries.extend(planned);
    Ok(())
}

/// The meters named `meter_name` reachable from a spend's ancestor chain, nearest
/// enforcing level first, each paired with its concrete enforcing row address
/// (§15.4).
fn reachable_levels<'a>(
    meters: &'a CompiledMeters,
    spend_address: &RowAddress,
    _decl: &[String],
    meter_name: &str,
) -> Vec<(&'a CompiledMeter, RowAddress)> {
    let steps: Vec<AddressStep> = spend_address.steps().cloned().collect();
    let mut out = Vec::new();
    for depth in (1..steps.len()).rev() {
        let ancestor = steps.iter().take(depth);
        let path: Vec<String> = ancestor.clone().map(|s| s.name().as_str().to_owned()).collect();
        if let (Some(meter), Some(address)) = (meters.meter_at(&path, meter_name), address_of(ancestor.cloned())) {
            out.push((meter, address));
        }
    }
    out
}

/// Rebuild the row address addressed by a prefix of a spend's address steps.
fn address_of(steps: impl Iterator<Item = AddressStep>) -> Option<RowAddress> {
    let mut steps = steps;
    let first = steps.next()?;
    let mut address = RowAddress::root(first);
    for step in steps {
        address = address.child(step);
    }
    Some(address)
}

/// The sum of allocations held against `(level, source, pool)` by every extant
/// spend row (§15.2 "allocations held by extant spend rows").
pub(crate) fn allocated(prospective: &Prospective, level: &str, source: &str, pool: &Value) -> BigDecimal {
    let mut total = zero();
    for fields in prospective.working().values() {
        for entry in decode(fields) {
            if entry.level == level && entry.source == source && entry.pool == *pool {
                total += entry.amount;
            }
        }
    }
    total
}

/// The declaration-name path of a row address (§5.4): the collection name of each
/// step, top to bottom.
fn decl_path(address: &RowAddress) -> Vec<String> {
    address.steps().map(|s| s.name().as_str().to_owned()).collect()
}

/// The `spend` binding cell for `$eligible` (§15.2/§15.6): the materialized spend
/// row with its config metadata folded in and the evaluated `$amount`/`$time`
/// structural cells `spend.$amount`/`spend.$time` read. `amount`/`time` are the
/// exact values already resolved for this consume, so eligibility sees the spend's
/// effective amount and time — not merely its raw `.amount`/`.occurred_at` fields,
/// which a config `$amount`/`$time` override may diverge from.
fn spend_context_cell(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    spend_row: &Row,
    consume: &SpendConsume,
    amount: &BigDecimal,
    time: Timestamp,
    now: Timestamp,
) -> Result<Cell, Rejection> {
    let mut cells: Vec<(String, Cell)> =
        spend_row.cells().map(|(name, cell)| (name.clone(), cell.clone())).collect();
    let base = Cell::Row(Box::new(spend_row.clone()));
    for (name, expr) in &consume.metadata {
        let env = ctx.env_at(prospective, now);
        let value = expr.evaluate(&env, &base).map_err(Rejection::from)?;
        set_cell(&mut cells, name, value);
    }
    set_cell(&mut cells, "$amount", Cell::Scalar(Value::Decimal(Decimal::from_big_decimal(amount.clone()))));
    set_cell(&mut cells, "$time", Cell::Scalar(Value::Timestamp(time)));
    Ok(Cell::Row(Box::new(Row::new(spend_row.id().clone(), spend_row.key().clone(), cells))))
}

fn set_cell(cells: &mut Vec<(String, Cell)>, name: &str, cell: Cell) {
    match cells.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = cell,
        None => cells.push((name.to_owned(), cell)),
    }
}

fn eval_decimal(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    expr: &liasse_expr::TypedExpr,
    current: &Cell,
    now: Timestamp,
) -> Result<BigDecimal, Rejection> {
    let env = ctx.env_at(prospective, now);
    match expr.evaluate(&env, current).map_err(Rejection::from)? {
        Cell::Scalar(value) => decimal_of(&value),
        _ => Err(Rejection::new(RejectionReason::TypeError, "spend amount is not a number")),
    }
}

fn eval_time(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    expr: &liasse_expr::TypedExpr,
    current: &Cell,
    now: Timestamp,
) -> Result<Timestamp, Rejection> {
    let env = ctx.env_at(prospective, now);
    match expr.evaluate(&env, current).map_err(Rejection::from)? {
        Cell::Scalar(Value::Timestamp(instant)) => Ok(instant),
        _ => Ok(now),
    }
}

/// Decode a spend row's frozen funding entries (§15.3).
fn decode(fields: &FieldMap) -> Vec<Entry> {
    let Some(Value::Set(entries)) = fields.get(FUNDING_FIELD) else { return Vec::new() };
    entries
        .iter()
        .filter_map(|entry| {
            let Value::Struct(members) = entry else { return None };
            let field = |name: &str| members.fields().find(|(n, _)| n.as_str() == name).map(|(_, v)| v);
            let level = match field("level") { Some(Value::Text(t)) => t.as_str().to_owned(), _ => return None };
            let source = match field("source") { Some(Value::Text(t)) => t.as_str().to_owned(), _ => return None };
            let pool = field("pool").cloned().unwrap_or(Value::None);
            let amount = match field("amount") { Some(Value::Decimal(d)) => d.as_big_decimal().clone(), _ => return None };
            Some(Entry { level, source, pool, amount })
        })
        .collect()
}

/// Encode funding entries into the stored `$funding` set value (§15.3).
fn encode(entries: &[Entry]) -> Value {
    Value::Set(
        entries
            .iter()
            .map(|entry| {
                Value::Struct(Struct::new([
                    (Text::new("level"), Value::Text(Text::new(entry.level.clone()))),
                    (Text::new("source"), Value::Text(Text::new(entry.source.clone()))),
                    (Text::new("pool"), entry.pool.clone()),
                    (Text::new("amount"), Value::Decimal(Decimal::from_big_decimal(entry.amount.clone()))),
                ]))
            })
            .collect(),
    )
}

/// The read-facing `funding` cell for a spend row (§15.6): the frozen allocation
/// projected to `{ source, pool, amount }`, dropping the internal enforcing
/// level. Used by [`accessor`](super::accessor).
pub(crate) fn funding_cell(fields: &FieldMap) -> Cell {
    let rows = decode(fields)
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            Row::new(
                RowId::leaf(index as u64),
                Value::None,
                [
                    ("source".to_owned(), Cell::Scalar(Value::Text(Text::new(entry.source)))),
                    ("pool".to_owned(), Cell::Scalar(entry.pool)),
                    ("amount".to_owned(), Cell::Scalar(Value::Decimal(Decimal::from_big_decimal(entry.amount)))),
                ],
            )
        })
        .collect();
    Cell::Collection(rows)
}

/// Exact decimal zero.
fn zero() -> BigDecimal {
    BigDecimal::from(0)
}
