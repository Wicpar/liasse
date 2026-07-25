//! The body of one admission transaction: what a commit actually writes.
//!
//! Kept apart from [`crate::store`] because it is a different concern. [`PgStore`]
//! is a handle — connections, the contract's reads, the lifecycle; this is the
//! single write an admission performs, and the *only* thing in the crate that
//! appends to durable state on behalf of a transition.
//!
//! It writes **state and a residual, nothing else**: the ops into the `nodes` tree
//! and a `commit_log` row with no serial position. It takes no lock and reads no
//! counter, which is what lets two admissions to one instance overlap. The position
//! is stamped later, over settled admissions, by [`crate::history`] (§22.1).
//!
//! Neither function begins or commits the transaction. The caller owns it, because
//! a folded multi-instance commit (§13.10) runs [`commit_body`] once per touched
//! schema on ONE shared transaction so every instance commits together or none does.
//!
//! # The write order below is a lock-order contract
//!
//! `commit_log`, then `nodes`, then `instance_meta` — the order `LOCK_ORDER` in
//! [`crate::schema`] records and the schema DDL is emitted in. It matters because an
//! admission is not the only thing that locks these tables: an opener reconciling
//! the same instance takes a `ShareLock` on each for the whole of its DDL
//! transaction. Two parties each taking two locks deadlock unless their orders
//! agree, so reordering the writes below means reordering `LOCK_ORDER` with them.
//!
//! [`PgStore`]: crate::store::PgStore

use liasse_ident::TransactionId;
use liasse_store::{CommittedRowOp, Composition, DefinitionText, PendingCommit, StoreError};
use liasse_value::Timestamp;
use postgres::Transaction;
use serde_json::Value as J;

use crate::backend::{backend, cell};
use crate::history;
use crate::jsonb_text;
use crate::node_write::NodeWriter;
use crate::record_codec::{encode_composition, encode_op};

/// Admit one instance's already-resolved ops into an OPEN SQL transaction against
/// its quoted `schema` (`s`), WITHOUT beginning or committing the transaction:
/// record the residual in `commit_log` and land the ops in the `nodes` tree.
/// Returns the admission's identity, which history later positions.
///
/// **No lock and no position are taken.** The caller owns the transaction: a
/// single-engine admission commits it alone; a folded multi-engine commit (§13.10)
/// runs this once per touched schema on ONE shared transaction, so every touched
/// instance commits together or the whole transaction rolls back — and, because
/// nothing here waits on anything instance-wide, two folded commits over overlapping
/// instances neither deadlock nor serialize. Assumes the payload is non-empty (the
/// empty case is [`liasse_store::CommitOutcome::Unchanged`], filtered before this is reached).
pub(crate) fn commit_body(
    txn: &mut Transaction<'_>,
    s: &str,
    ops: &[CommittedRowOp],
    created: Timestamp,
    transaction: Option<&TransactionId>,
    definition: Option<&DefinitionText>,
    composition: Option<&Composition>,
) -> Result<history::AdmissionId, StoreError> {
    // Neither `jsonb` nor a raw `text` column can hold a `U+0000`, which a valid
    // `text` value/key or an unvalidated D.5 token (transaction id) or D.4 source
    // may carry; NUL-safe-encode every string leaf before it reaches a column.
    let transaction_id = transaction.map(|t| jsonb_text::encode_text(t.as_str()));
    let ops_wire = jsonb_text::to_jsonb(&J::Array(ops.iter().map(encode_op).collect()));
    // §22.5/§22.6: the commit's fixed `now` — the `$created` every inserted row
    // records — persisted so a log-fold replay reconstructs it (§14.1).
    let created_wire = jsonb_text::to_jsonb(&crate::value_codec::encode_created(created));
    let definition_source = definition.map(|d| jsonb_text::encode_text(d.source()));
    let definition_id = definition.map(|d| d.identity().to_canonical_text());
    let composition_wire = composition.map(|c| jsonb_text::to_jsonb(&encode_composition(c)));

    // The residual: what this admission did, with no position yet. `xid` defaults to
    // this transaction's own id, which is its identity and the order history will
    // build it in — so the record is written without reading, locking, or waiting for
    // anything instance-wide.
    let admitted = txn
        .query_one(
            &format!(
                "INSERT INTO {s}.commit_log (transaction_id, ops, created) VALUES ($1, $2, $3) \
                 RETURNING xid::text AS admission"
            ),
            &[&transaction_id, &ops_wire, &created_wire],
        )
        .map_err(backend)?;
    let admission = history::AdmissionId::new(cell::<String>(&admitted, "commit_log", "admission")?);
    // Every op lands in the `nodes` adjacency tree — the sole durable row
    // representation — in this one admission transaction. `NodeWriter` resolves
    // each address to its surrogate id by an in-transaction SQL point lookup
    // (§6.1), so nodes inserted earlier in this same admission are visible; there
    // is no `by_id` projection to advance afterward. It carries the commit's
    // `now` so a fresh insert stamps the row's `$created` (§14.1, §22.6).
    let mut node_writer = NodeWriter::new(s, created);
    for op in ops {
        node_writer.apply(txn, op)?;
    }
    // `instance_meta` now holds only instance-wide singletons, and is touched ONLY
    // when this transition actually changes one. A plain data mutation therefore
    // never writes it at all — which is what keeps two data admissions from meeting
    // on any shared row.
    if definition.is_some() || composition.is_some() {
        txn.execute(
            &format!(
                "UPDATE {s}.instance_meta SET \
                 definition_source = COALESCE($1, definition_source), \
                 definition_id = COALESCE($2, definition_id), \
                 composition = COALESCE($3, composition) WHERE id = 1"
            ),
            &[&definition_source, &definition_id, &composition_wire],
        )
        .map_err(backend)?;
    }
    Ok(admission)
}

/// Admit one participant's payload into an open transaction, mapping an empty
/// payload to no admission at all (§22.2, [`liasse_store::CommitOutcome::Unchanged`]).
pub(crate) fn commit_member(
    txn: &mut Transaction<'_>,
    s: &str,
    pending: &PendingCommit,
) -> Result<Option<history::AdmissionId>, StoreError> {
    if pending.ops.is_empty() && pending.definition.is_none() && pending.composition.is_none() {
        return Ok(None);
    }
    commit_body(
        txn,
        s,
        &pending.ops,
        pending.created,
        pending.transaction.as_ref(),
        pending.definition.as_ref(),
        pending.composition.as_ref(),
    )
    .map(Some)
}
