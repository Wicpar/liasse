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
//! creation instants).
//!
//! # Temporal selectors (§14.1–§14.2)
//!
//! The `.$at(t)`/`.$between(a, b)`/`.$all` selectors resolve through
//! [`interval_bounds`]: each bucketed row carries its evaluated `[from, until)`
//! as the `$from`/`$until` structural cells, and [`RuntimeEnv`](crate::env)
//! answers [`Environment::temporal`](liasse_expr::Environment::temporal) by
//! applying §14.1's half-open activity rule to those cells. A bare read of a
//! bucketed collection still yields only the rows active at the clock (via
//! [`is_active`]); a selector re-derives activity from the full extant set, so
//! `.$all` and a back-dated `.$at` observe rows that have already left the active
//! interval (§14.2).
//!
//! # Documented seams
//!
//! Source-backed and recurring buckets (`$source`/`$repeat`, §14.4–§14.6) and
//! bounds that read other root collections need source derivation the CORE
//! materializer does not yet build; recurring calendar periods additionally need
//! period-to-timestamp arithmetic `liasse-value` does not expose.

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

/// The half-open interval `[from, until)` of a bucketed row at `now`, with an
/// unevaluable bound treated as unconstrained (`None`) — the same fail-open
/// stance as [`is_active`]. This is the temporal index a [`TemporalQuery`] reads:
/// the runtime evaluates each bucketed row's bounds once and exposes them as the
/// `$from`/`$until` structural cells the selector filters on (§14.1, §14.4).
///
/// [`TemporalQuery`]: liasse_expr::TemporalQuery
#[must_use]
pub(crate) fn interval_bounds(
    bucket: &CompiledBucket,
    collection: &CompiledCollection,
    fields: &FieldMap,
    now: Timestamp,
) -> (Option<Timestamp>, Option<Timestamp>) {
    bounds(bucket, collection, fields, now).unwrap_or((None, None))
}

/// Reject a transition that produced an invalid finite interval (§14.2): a
/// present, finite `$until` MUST be strictly after a present, finite `$from`.
///
/// A `$created`-defaulted (or omitted) `$from` carries no stored lower-bound
/// expression, yet at admission the row's creation instant is the request's
/// fixed `now()` (§A.5). The lower bound the new row will carry is therefore
/// `now`, so a `$until` at or before `now` yields the empty interval `[now, now)`
/// and MUST reject — this is the minimum-lifetime boundary (§14.2): a session
/// whose `expires_at` equals its own admission instant is never active.
pub(crate) fn check_interval(
    bucket: &CompiledBucket,
    collection: &CompiledCollection,
    fields: &FieldMap,
    now: Timestamp,
    where_path: &str,
) -> Result<(), Rejection> {
    let (from, until) = bounds(bucket, collection, fields, now).map_err(Rejection::from)?;
    let from = from.unwrap_or(now);
    if let Some(until) = until
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
    let env = RuntimeEnv::new(
        Row::keyless(RowId::leaf(0), Vec::new()),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        now,
        0,
        Vec::new(),
    );
    match typed.evaluate(&env, &current)? {
        Cell::Scalar(Value::Timestamp(ts)) => Ok(Some(ts)),
        _ => Ok(None),
    }
}
