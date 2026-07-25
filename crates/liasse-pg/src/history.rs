//! History construction: turning settled admissions into positioned history.
//!
//! §22.1 makes this a separate job from writing: *"History construction follows
//! committed transitions independently of write admission. Independent writes may be
//! captured concurrently."* So an admission writes state and a bare record of what it
//! did — its **residual**, a `commit_log` row with `seq` NULL — and takes **no serial
//! position and no lock**. This module is the second half: it reads the residuals
//! that have settled and stamps the serial positions §22.3 requires, in one order,
//! after the fact.
//!
//! # Why the position cannot be taken during admission
//!
//! Two designs remove the per-instance write lock and both are wrong in the same
//! way if the position is minted before the transaction commits: a writer holding
//! position `N` can commit *after* one holding `N+1`, so a reader scanning "everything
//! up to head" sees `N+1`, misses `N`, and then watches `N` appear in its own past.
//! That breaks the property §22.3 declares for this implementation — *"an operation
//! issued after an observed commit is ordered after it"*. Assigning the position
//! after settlement removes the window rather than papering over it: at the moment a
//! position is stamped, the transaction that gets it has already committed.
//!
//! # The watermark is where the correctness lives
//!
//! A pass may only position an admission that **can no longer be beaten by a
//! straggler** — an older transaction still in flight, which would otherwise surface
//! later and demand an earlier position than one already handed out. The boundary is
//! the current snapshot's `xmin` ([`pg_snapshot_xmin`] of [`pg_current_snapshot`]):
//! the oldest transaction id still running. Every transaction whose id is *below* it
//! has already ended — committed or aborted — and no transaction that starts from now
//! on can be assigned an id below it either. So residuals with `xid < xmin` are a
//! complete, final prefix in `xid` order, and residuals at or above it are left for a
//! later pass.
//!
//! Note what this buys: the pass does not merely skip a straggler, it **stops at**
//! it. A residual admitted *after* an in-flight one is held back too, so history never
//! shows a later admission before an earlier one that is still settling. That is the
//! anomaly, gone by construction rather than by mitigation.
//!
//! # Ordering, and what it means
//!
//! Positions follow `xid` — the order the admissions *began writing*, which is the
//! ingress order §22.3 explicitly permits as an ordering event ("an assigned ingress
//! sequence"). Two genuinely concurrent admissions have no path between them, so
//! either relative order is valid (§22.3), and this picks one deterministically. The
//! declared client-coherence property survives because an operation a client issues
//! *after* observing a commit necessarily starts writing later, so it takes a higher
//! `xid` — and because the watermark guarantees the observed commit was already
//! positioned before it could be observed at all.
//!
//! # Gaps
//!
//! Positions come out gapless, but that is a *consequence*, not a requirement (§22.3
//! asks only for monotonicity): a residual is written inside the admitting
//! transaction, so an aborted admission leaves none behind and there is nothing to
//! skip. Nothing here works to preserve gaplessness and nothing may depend on it.
//!
//! # The one remaining lock, and what it is not
//!
//! Two passes running at once would both read the same current tip and hand out the
//! same positions, so a pass takes a transaction-scoped **advisory** lock keyed by
//! the schema. It serializes *history construction*, which is inherently the
//! building of one linear order; it is never taken by an admission, so two
//! admissions still never wait for each other.
//!
//! [`pg_snapshot_xmin`]: https://www.postgresql.org/docs/17/functions-info.html
//! [`pg_current_snapshot`]: https://www.postgresql.org/docs/17/functions-info.html

use liasse_store::{CommitSeq, StoreError};
use postgres::Client;

use crate::backend::{backend, cell};
use crate::record_codec::seq_from;
use crate::schema::Schema;

/// One admission's identity: the id of the transaction that admitted it, carried as
/// its canonical decimal text so it round-trips through the driver without an
/// `xid8` type binding.
///
/// It is minted by PostgreSQL at admission (`pg_current_xact_id()`), never reused,
/// and is both the `commit_log` primary key and the order history is built in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdmissionId(String);

impl AdmissionId {
    /// Adopt the id PostgreSQL returned from the admitting `INSERT`.
    pub(crate) fn new(text: String) -> Self {
        Self(text)
    }

    /// The id as the `xid8`-castable text a query binds.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Run one history-construction pass over `schema`: position every settled,
/// still-unpositioned admission, in admitting-transaction order, starting from the
/// current tip. Returns the number of admissions positioned.
///
/// Idempotent and safe to call at any time from any connection — a pass with nothing
/// to do positions nothing. The advisory lock is transaction-scoped, so it is
/// released by the `COMMIT` below whatever happens.
pub(crate) fn build(client: &mut Client, schema: &Schema) -> Result<u64, StoreError> {
    let s = schema.quoted();
    let mut txn = client.transaction().map_err(backend)?;
    // Serialize passes against each other (never against admissions): two passes
    // would otherwise read the same tip and mint the same positions.
    txn.execute("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", &[&schema.name()])
        .map_err(backend)?;
    // One statement, so the tip it counts from and the rows it stamps are the same
    // snapshot. `row_number()` over `xid` lays the settled prefix down in ingress
    // order immediately after the current tip.
    let positioned = txn
        .execute(
            &format!(
                "WITH settled AS ( \
                     SELECT xid, row_number() OVER (ORDER BY xid) AS offset_from_tip \
                     FROM {s}.commit_log \
                     WHERE seq IS NULL AND xid < pg_snapshot_xmin(pg_current_snapshot()) \
                 ), tip AS (SELECT coalesce(max(seq), 0) AS at FROM {s}.commit_log) \
                 UPDATE {s}.commit_log AS log \
                 SET seq = tip.at + settled.offset_from_tip \
                 FROM settled, tip \
                 WHERE log.xid = settled.xid"
            ),
            &[],
        )
        .map_err(backend)?;
    txn.commit().map_err(backend)?;
    Ok(positioned)
}

/// The serial position `admission` was given, or `None` while it is still waiting
/// for a pass to reach it (an older admission is in flight).
pub(crate) fn position_of(
    client: &mut Client,
    s: &str,
    admission: &AdmissionId,
) -> Result<Option<CommitSeq>, StoreError> {
    let row = client
        .query_opt(
            // `$1::text::xid8`, not `$1::xid8`: the double cast pins the *parameter*
            // to `text` (which the driver can serialize) and converts it to `xid8`
            // for the comparison, instead of asking the driver to send an `xid8`.
            &format!("SELECT seq FROM {s}.commit_log WHERE xid = $1::text::xid8"),
            &[&admission.as_str()],
        )
        .map_err(backend)?;
    let seq = row
        .ok_or_else(|| StoreError::Corruption {
            detail: format!("admission {} has no commit_log record", admission.as_str()),
        })?
        .try_get::<_, Option<i64>>("seq")
        .map_err(backend)?;
    seq.map(|raw| seq_from(raw, "commit_log.seq")).transpose()
}

/// The built history's tip: the highest position a pass has handed out, or
/// [`CommitSeq::GENESIS`] before the first one. This — not any write-side counter —
/// is the instance's head.
pub(crate) fn tip(
    conn: &mut impl postgres::GenericClient,
    s: &str,
) -> Result<CommitSeq, StoreError> {
    let row = conn
        .query_one(&format!("SELECT coalesce(max(seq), 0) AS tip FROM {s}.commit_log"), &[])
        .map_err(backend)?;
    seq_from(cell::<i64>(&row, "commit_log", "tip")?, "commit_log.seq")
}

/// Whether any admission has committed state that history has not positioned yet —
/// the observable gap between committed state and built history. A reader that must
/// see exactly the state history describes checks this and falls back to the log
/// fold when it is true.
pub(crate) fn has_unpositioned(
    conn: &mut impl postgres::GenericClient,
    s: &str,
) -> Result<bool, StoreError> {
    let row = conn
        .query_one(
            &format!("SELECT EXISTS(SELECT 1 FROM {s}.commit_log WHERE seq IS NULL) AS pending"),
            &[],
        )
        .map_err(backend)?;
    cell::<bool>(&row, "commit_log", "pending")
}
