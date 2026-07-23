//! Node-adjacency write path: apply every committed row op to the `nodes`
//! adjacency tree by surrogate id.
//!
//! The `nodes` tree is the sole durable representation of committed rows, and this
//! module is the only writer. Every op is applied by the node's surrogate id,
//! resolved **in the admission transaction itself** by the same chained point
//! lookup the read path uses ([`crate::read::resolve_id`], §6.1) — so a node this
//! transaction inserted earlier is visible to a later op's resolution — and
//! memoized in a per-transaction map ([`NodeWriter::staged`]). There is no
//! committed `by_id` projection to consult: durable node identity lives only in the
//! `nodes` table (`DESIGN-pure-pg.md` §6.1).
//!
//! # A node is a position; a row is a node with a value
//!
//! `value`/`incarnation` are nullable: a **live row** has both non-NULL, a
//! **tombstone** has both NULL. A tombstone is a structural-only position — a
//! deleted non-leaf ancestor kept solely so its descendant rows stay addressable —
//! never itself observed as a row. This is what lets the durable tree represent the
//! state the runtime actually produces on a top-level drop (§21.1): the top-level
//! row is deleted while its nested rows are left as *logical orphans* (§5.4), still
//! at their addresses. The earlier design cascade-deleted the whole subtree here,
//! which dropped those orphans — an [observable divergence from the reference store
//! across a reopen](../../tests/redteam_cascade_delete_orphan_reopen.rs), since the
//! reference store (removing only the addressed row) and the reopened tree then
//! disagreed. Tombstoning removes that divergence.
//!
//! Every op is applied **by the surrogate node id**, resolved from the
//! per-transaction memo ([`NodeWriter::staged`]) first, then by an in-transaction
//! SQL point lookup over `(parent_id, step_name, key_enc)` that finds the node
//! whether it is a live row or a tombstone (so a nested op resolves its parent even
//! under a tombstoned ancestor, §5.4):
//!
//! - **Insert** creates a node under its parent (the root sentinel `0` at depth 1,
//!   else the parent row's node) and returns its id. `ON CONFLICT DO UPDATE` makes
//!   an insert at a *tombstoned* address REVIVE that node in place, keeping its id so
//!   any descendants it retained are re-parented under the live row again — matching
//!   the reference store, which allocates a fresh incarnation on re-insert. (A valid
//!   op stream never inserts over a *live* row; admission staging rejects that.) If a
//!   nested insert's ancestor chain was NEVER created, the missing ancestors are
//!   AUTO-CREATED as tombstones so the semantics-free store admits the row exactly as
//!   the reference does (its only precondition is occupancy, never ancestor
//!   existence, §5.4) — the ancestors stay absent as rows, so reads still match
//!   `MemoryStore` live and after a reopen ([`NodeWriter::resolve_parent`]).
//! - **Update** rewrites the resolved node's value/incarnation in place.
//! - **Delete** TOMBSTONES the resolved node (`value = NULL, incarnation = NULL`) and
//!   leaves its descendants untouched, so a nested row whose ancestor was dropped
//!   survives as an orphan — exactly the reference-store semantics. No subtree is
//!   cascaded. A fully-dead subtree (a tombstone with no live descendant) is inert
//!   and a future GC opportunity; retaining it is correctness-neutral.
//! - **Rekey** moves ONLY the addressed row (§5.4 reference semantics): it places the
//!   row's value/incarnation at the target address (reviving a tombstone there or
//!   creating a fresh node, same as Insert) and TOMBSTONES the source node, so the
//!   source's descendants remain orphans under the source address. The subtree is
//!   NOT id-moved — moving it would relocate descendants the reference store leaves
//!   in place.
//!
//! `key_enc` (the order-preserving `BYTEA`) is written for lookup/scan order;
//! `key_wire` (canonical, self-describing JSONB) is written so a load can decode
//! the level key back into a [`KeyValue`] and reconstruct the address.

use std::collections::BTreeMap;

use liasse_ident::RowIncarnation;
use liasse_store::{AddressStep, CommittedRowOp, KeyValue, RowAddress, StoreError};
use liasse_value::{Timestamp, Value};
use postgres::Transaction;
use serde_json::Value as J;

use crate::backend::{backend, cell, corrupt};
use crate::jsonb_text;
use crate::key_enc;
use crate::read;
use crate::value_codec;

/// The self-referential root sentinel: the `parent_id` of every depth-1 node and
/// the terminus of every parent-walk.
const ROOT_SENTINEL_ID: i64 = 0;

/// Applies a commit's row ops to the `nodes` table by surrogate id, resolving each
/// address to its node id by an in-transaction SQL point lookup (§6.1) memoized in
/// `staged` — the ids this transaction has itself just created, moved, or resolved.
pub(crate) struct NodeWriter<'a> {
    /// The quoted schema name, e.g. `"liasse_…"`.
    schema: &'a str,
    /// The commit's fixed admission instant (§22.5): the `$created` a fresh insert
    /// stamps on its row (§14.1, §22.6). An update leaves the row's `created`; a
    /// rekey carries the source's — so it is recorded once, matching the reference.
    now: Timestamp,
    /// Per-transaction id memo: addresses inserted, rekeyed-into, or resolved during
    /// THIS transaction, and their surrogate ids. Consulted before the SQL point
    /// lookup so a nested child sees its just-inserted parent without a re-query.
    staged: BTreeMap<RowAddress, i64>,
}

impl<'a> NodeWriter<'a> {
    /// Open a writer for one admission transaction at commit instant `now`. Node
    /// identity is resolved from the `nodes` table in that same transaction — there
    /// is no committed side map.
    pub(crate) fn new(schema: &'a str, now: Timestamp) -> Self {
        Self { schema, now, staged: BTreeMap::new() }
    }

    /// Mirror one committed op into the node tree.
    pub(crate) fn apply(
        &mut self,
        txn: &mut Transaction<'_>,
        op: &CommittedRowOp,
    ) -> Result<(), StoreError> {
        match op {
            CommittedRowOp::Insert { address, incarnation, value } => {
                // §14.1/§22.6: a fresh insert stamps the commit's `now` as `$created`.
                self.place(txn, address, incarnation, self.now, value)?;
            }
            CommittedRowOp::Update { address, incarnation, value } => {
                // §22.6: an update rewrites value/incarnation but leaves `created`, so
                // the row keeps its first-recorded `$created`.
                let id = self.resolve_id(txn, address)?;
                txn.execute(
                    &format!(
                        "UPDATE {}.nodes SET value = $1, incarnation = $2 WHERE id = $3",
                        self.schema
                    ),
                    &[
                        &jsonb_text::to_jsonb(&value_codec::encode(value)),
                        &incarnation.as_str(),
                        &id,
                    ],
                )
                .map_err(backend)?;
            }
            CommittedRowOp::Delete { address, .. } => {
                let id = self.resolve_id(txn, address)?;
                self.tombstone(txn, id)?;
                self.staged.remove(address);
            }
            CommittedRowOp::Rekey { from, to, incarnation, value } => {
                // Move ONLY the addressed row (§5.4): place its value/incarnation at
                // the target (reviving a tombstone there or creating fresh), then
                // tombstone the source so its descendants stay orphans under the
                // source address. The subtree is NOT id-moved — the reference store
                // leaves those descendants where they are. The op carries the
                // source's preserved incarnation, so the target reads back the same
                // incarnation the reference store keeps across a rekey.
                //
                // §22.6: a rekey keeps identity, so the target carries the source's
                // recorded `$created` (read before the source is tombstoned), never a
                // fresh `now` — matching the reference store's rekey.
                let from_id = self.resolve_id(txn, from)?;
                let created = self.node_created(txn, from_id)?.unwrap_or(self.now);
                self.place(txn, to, incarnation, created, value)?;
                self.tombstone(txn, from_id)?;
                self.staged.remove(from);
            }
        }
        Ok(())
    }

    /// The recorded `$created` of the live node `id`, if it carries one — the
    /// source instant a rekey preserves onto its target (§22.6). `None` for a
    /// tombstone (a rekey source is always a live row, so the caller falls back to
    /// the commit `now` only defensively).
    fn node_created(
        &self,
        txn: &mut Transaction<'_>,
        id: i64,
    ) -> Result<Option<Timestamp>, StoreError> {
        let row = txn
            .query_one(&format!("SELECT created FROM {}.nodes WHERE id = $1", self.schema), &[&id])
            .map_err(backend)?;
        match cell::<Option<J>>(&row, "nodes", "created")? {
            Some(wire) => Ok(Some(value_codec::decode_created(&jsonb_text::from_jsonb(&wire))?)),
            None => Ok(None),
        }
    }

    /// Place `value`/`incarnation` at `address` — reviving a tombstone there or
    /// creating a fresh node — and record its surrogate id. The unique
    /// `node_key_lookup` index is the `ON CONFLICT` arbiter, so a re-place at a
    /// tombstoned address updates that same node in place, keeping its id and with it
    /// any descendants it retained. A valid op stream never places over a *live* row
    /// (admission staging rejects an insert/rekey onto an occupied address), so the
    /// conflict target is always either free or a tombstone.
    fn place(
        &mut self,
        txn: &mut Transaction<'_>,
        address: &RowAddress,
        incarnation: &RowIncarnation,
        created: Timestamp,
        value: &Value,
    ) -> Result<i64, StoreError> {
        let parent = self.resolve_parent(txn, address)?;
        let step = last_step(address)?;
        let row = txn
            .query_one(
                &format!(
                    "INSERT INTO {}.nodes \
                     (parent_id, step_name, key_enc, key_wire, incarnation, value, created) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7) \
                     ON CONFLICT (parent_id, step_name, key_enc) \
                     DO UPDATE SET incarnation = EXCLUDED.incarnation, value = EXCLUDED.value, \
                     created = EXCLUDED.created \
                     RETURNING id",
                    self.schema
                ),
                &[
                    &parent,
                    &step.name().as_str(),
                    &key_enc::encode_key_value(step.key()),
                    &jsonb_text::to_jsonb(&encode_key_wire(step.key())),
                    &incarnation.as_str(),
                    &jsonb_text::to_jsonb(&value_codec::encode(value)),
                    &jsonb_text::to_jsonb(&value_codec::encode_created(created)),
                ],
            )
            .map_err(backend)?;
        let id = cell::<i64>(&row, "nodes", "id")?;
        self.staged.insert(address.clone(), id);
        Ok(id)
    }

    /// Tombstone the node `id`: null its value/incarnation so it is no longer a row,
    /// while leaving the node — and every descendant hanging off it — in place. This
    /// is how a deletion retains logical orphans (§21.1/§5.4): a nested row survives
    /// its ancestor's deletion, addressable through the tombstone. A fully-dead
    /// subtree (a tombstone with no live descendant) is inert and a future GC
    /// opportunity; retaining it is correctness-neutral.
    fn tombstone(&self, txn: &mut Transaction<'_>, id: i64) -> Result<(), StoreError> {
        txn.execute(
            &format!(
                "UPDATE {}.nodes SET value = NULL, incarnation = NULL, created = NULL WHERE id = $1",
                self.schema
            ),
            &[&id],
        )
        .map_err(backend)?;
        Ok(())
    }

    /// The node id of `address`: the per-transaction memo first, then an
    /// in-transaction SQL point lookup (§6.1) — memoized on hit. The lookup finds a
    /// node whether live or tombstoned, exactly as the former `by_id` map did. A miss
    /// means an op referenced a row with no node — a durable inconsistency.
    fn resolve_id(
        &mut self,
        txn: &mut Transaction<'_>,
        address: &RowAddress,
    ) -> Result<i64, StoreError> {
        if let Some(id) = self.staged.get(address).copied() {
            return Ok(id);
        }
        match read::resolve_id(txn, self.schema, address)? {
            Some(id) => {
                self.staged.insert(address.clone(), id);
                Ok(id)
            }
            None => Err(corrupt(format!("no node for row address {}", address.render()))),
        }
    }

    /// The parent node id of `address`: the root sentinel for a top-level row, else
    /// the node one level up — AUTO-CREATED as a structural-only tombstone if it (or
    /// any ancestor above it) was never inserted.
    ///
    /// The store contract is semantics-free: `Transition::insert`'s only precondition
    /// is occupancy (Conflict on the row's own address), never ancestor existence, so
    /// the reference `MemoryStore` — a flat `RowAddress`→row map — admits a nested row
    /// like `/orgs/1/teams/10` whether or not `/orgs/1` ever existed (§5.4 gives a
    /// nested row its own identity plus ancestor identity; a "logical orphan" is
    /// exactly an addressable descendant of an absent ancestor). The node tree resolves
    /// a row's parent by surrogate id, so to admit the same op it MATERIALIZES the
    /// missing ancestor chain as tombstones (value/incarnation NULL) rather than
    /// erroring. The ancestors stay ABSENT AS ROWS — a tombstone is a structural
    /// position, never emitted into `current` — so `row(/orgs/1)`/`scan(/orgs)` stay
    /// identical to `MemoryStore`, live and after a reopen; only `scan(/orgs/1/teams)`
    /// sees the placed row. A later explicit `insert(/orgs/1)` REVIVES the tombstone
    /// through [`Self::place`]'s existing `ON CONFLICT` path.
    fn resolve_parent(
        &mut self,
        txn: &mut Transaction<'_>,
        address: &RowAddress,
    ) -> Result<i64, StoreError> {
        match parent_address(address) {
            None => Ok(ROOT_SENTINEL_ID),
            Some(parent) => self.resolve_or_create(txn, &parent),
        }
    }

    /// Resolve `address`'s node id — the memo, then an in-transaction SQL point
    /// lookup — or, when it has no node, AUTO-CREATE it (and recursively any missing
    /// ancestor above it, down from the deepest existing ancestor or the root
    /// sentinel) as a structural-only tombstone. The lookup finds any existing node,
    /// live row or tombstone, so this only ever reaches a free address or an existing
    /// tombstone — never a live row it could clobber.
    fn resolve_or_create(
        &mut self,
        txn: &mut Transaction<'_>,
        address: &RowAddress,
    ) -> Result<i64, StoreError> {
        if let Some(id) = self.staged.get(address).copied() {
            return Ok(id);
        }
        if let Some(id) = read::resolve_id(txn, self.schema, address)? {
            self.staged.insert(address.clone(), id);
            return Ok(id);
        }
        let parent = match parent_address(address) {
            None => ROOT_SENTINEL_ID,
            Some(grandparent) => self.resolve_or_create(txn, &grandparent)?,
        };
        self.create_tombstone(txn, parent, address)
    }

    /// Insert a structural-only tombstone node (value/incarnation NULL — the existing
    /// tombstone representation, honoring the `CHECK ((value NULL) = (incarnation
    /// NULL))` invariant) for `address` under `parent`, or return the id of the node
    /// already there. The `ON CONFLICT DO UPDATE` is a no-op that only forces
    /// `RETURNING id` on an existing node — it never touches value/incarnation, so an
    /// already-present tombstone (or the impossible live row) is left intact rather
    /// than clobbered.
    ///
    /// The id is recorded in `staged` (so the rest of THIS transaction resolves the
    /// ancestor without re-querying). The auto-created ancestor lives in the durable
    /// tree (descendants stay addressable; a later insert revives it through
    /// [`Self::place`]'s `ON CONFLICT` path) and is re-derived idempotently through
    /// this same lookup-or-create path if a later nested insert needs it again before
    /// an explicit insert makes it live.
    fn create_tombstone(
        &mut self,
        txn: &mut Transaction<'_>,
        parent: i64,
        address: &RowAddress,
    ) -> Result<i64, StoreError> {
        let step = last_step(address)?;
        let row = txn
            .query_one(
                &format!(
                    "INSERT INTO {}.nodes \
                     (parent_id, step_name, key_enc, key_wire, incarnation, value) \
                     VALUES ($1, $2, $3, $4, NULL, NULL) \
                     ON CONFLICT (parent_id, step_name, key_enc) \
                     DO UPDATE SET step_name = EXCLUDED.step_name \
                     RETURNING id",
                    self.schema
                ),
                &[
                    &parent,
                    &step.name().as_str(),
                    &key_enc::encode_key_value(step.key()),
                    &jsonb_text::to_jsonb(&encode_key_wire(step.key())),
                ],
            )
            .map_err(backend)?;
        let id = cell::<i64>(&row, "nodes", "id")?;
        self.staged.insert(address.clone(), id);
        Ok(id)
    }
}

/// The final (own-collection) level of an address — the step this node stores.
/// A [`RowAddress`] is non-empty by construction, so this only fails on corruption.
fn last_step(address: &RowAddress) -> Result<&AddressStep, StoreError> {
    address.steps().last().ok_or_else(|| corrupt("row address has no steps"))
}

/// The address of a row's parent — itself minus its final level — or `None` when
/// the row is top-level (its parent is the root sentinel).
fn parent_address(address: &RowAddress) -> Option<RowAddress> {
    let mut steps: Vec<AddressStep> = address.steps().cloned().collect();
    steps.pop();
    let mut levels = steps.into_iter();
    let first = levels.next()?;
    let mut parent = RowAddress::root(first);
    for step in levels {
        parent = parent.child(step);
    }
    Some(parent)
}

/// The `key_wire` column for a level: the key's components in a canonical,
/// self-describing form ([`crate::node_load`] decodes it back into the address on
/// load).
fn encode_key_wire(key: &KeyValue) -> J {
    J::Array(key.components().map(value_codec::encode_key).collect())
}
