//! Bucket temporal activity (SPEC.md §14): a bucketed row is active at instant
//! `t` iff `t` lies in its half-open interval `[from, until)`.
//!
//! A `$bucket` associates a row with a logical interval. The engine's virtual
//! clock supplies the evaluation instant, so ordinary reads and view evaluation
//! expose only the rows active at the clock (§14.1, §14.2): a row leaves every
//! active view at the exact `until` instant (the interval is half-open) yet
//! remains extant until an explicit deletion.
//!
//! # CORE scope
//!
//! Lifecycle buckets over a top-level keyed collection are handled: the short
//! form (`"$bucket": ".expires_at"`, meaning `$until` with `$from` = the row's
//! admission) and the explicit `{ $from, $until }` object whose bounds are
//! `timestamp`/`timestamp?` expressions over the collection row. An omitted or
//! `$created`-defaulted `$from` is treated as "active from creation": a committed
//! row was admitted in the past and the clock only advances, so its lower bound
//! is always satisfied without depending on per-row admission-time storage (a
//! documented seam, since the store records serial positions, not wall-clock
//! creation instants). Source-backed and recurring buckets (`$source`/`$repeat`,
//! §14.4–§14.6), the `.$at`/`.$between`/`.$all` temporal selectors (§14.1), and
//! bounds that read other root collections are documented seams: those need
//! expression-layer selector support and source derivation the CORE checker does
//! not expose.

use std::collections::BTreeMap;

use liasse_expr::{Cell, EvalError, Row, RowId, TypedExpr};
use liasse_value::{Timestamp, Value};

use crate::compiled::{CompiledBucket, CompiledCollection};
use crate::env::RuntimeEnv;
use crate::error::{Rejection, RejectionReason};
use crate::eval::row_cell;
use crate::materialize::FieldMap;

/// Whether the bucketed row `fields` is active at `now` (§14.1): its lower bound
/// has been reached and its upper bound has not. A bound that fails to evaluate
/// is treated conservatively as unconstraining (fail-open) so a read never
/// silently drops a row on an evaluation fault; a genuinely invalid interval is
/// rejected at admission instead (see [`check_interval`]).
#[must_use]
pub(crate) fn is_active(
    bucket: &CompiledBucket,
    collection: &CompiledCollection,
    fields: &FieldMap,
    now: Timestamp,
) -> bool {
    match bounds(bucket, collection, fields, now) {
        Ok((from, until)) => {
            let started = from.is_none_or(|from| now >= from);
            let ended = until.is_some_and(|until| now >= until);
            started && !ended
        }
        Err(_) => true,
    }
}

/// Reject a transition that produced an invalid finite interval (§14.2): a
/// present, finite `$until` MUST be strictly after a present, finite `$from`.
pub(crate) fn check_interval(
    bucket: &CompiledBucket,
    collection: &CompiledCollection,
    fields: &FieldMap,
    now: Timestamp,
    where_path: &str,
) -> Result<(), Rejection> {
    let (from, until) = bounds(bucket, collection, fields, now).map_err(Rejection::from)?;
    if let (Some(from), Some(until)) = (from, until)
        && until <= from
    {
        return Err(Rejection::new(
            RejectionReason::Evaluation,
            "a bucket interval must end strictly after it starts",
        )
        .at(where_path.to_owned()));
    }
    Ok(())
}

/// Evaluate the interval bounds of a bucketed row against `now`. Each bound is a
/// `timestamp`/`timestamp?` expression over the collection row; an absent bound
/// or a `none` result is [`None`] (unconstrained / unbounded).
fn bounds(
    bucket: &CompiledBucket,
    collection: &CompiledCollection,
    fields: &FieldMap,
    now: Timestamp,
) -> Result<(Option<Timestamp>, Option<Timestamp>), EvalError> {
    let from = match &bucket.from {
        Some(expr) => eval_bound(expr, collection, fields, now)?,
        None => None,
    };
    let until = match &bucket.until {
        Some(expr) => eval_bound(expr, collection, fields, now)?,
        None => None,
    };
    Ok((from, until))
}

/// Evaluate one bound expression with the row as `.`. Bounds read row fields and
/// `now()`; the package root is unused in CORE scope, so an empty root suffices
/// and keeps this independent of the materialized (possibly filtered) root.
fn eval_bound(
    typed: &TypedExpr,
    collection: &CompiledCollection,
    fields: &FieldMap,
    now: Timestamp,
) -> Result<Option<Timestamp>, EvalError> {
    let current = row_cell(collection, fields);
    let env = RuntimeEnv::new(Row::keyless(RowId::leaf(0), Vec::new()), BTreeMap::new(), now, 0);
    match typed.evaluate(&env, &current)? {
        Cell::Scalar(Value::Timestamp(ts)) => Ok(Some(ts)),
        _ => Ok(None),
    }
}
