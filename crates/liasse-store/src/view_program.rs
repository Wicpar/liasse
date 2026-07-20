//! The evaluated read (§7 of `liasse-pg/DESIGN-pure-pg.md`): the one
//! semantics-carrying read of the contract, carried **opaquely** so `liasse-store`
//! stays semantics-free and gains no dependency on the expression layer.
//!
//! A [`ViewProgram`] is a compiled per-row evaluation program — the admit filter,
//! the projection, and the sort-tuple evaluation of one lowered view read. Its
//! three logical faces ([`ViewProgram::admits`]/[`ViewProgram::project`]/
//! [`ViewProgram::sort_tuple`]) are total over `(stored payload, typed key,
//! prefetched subtree)`; an evaluation fault is an [`EvalFault`], never a silent
//! verdict or a guessed value. The single implementor is `liasse_pred::RowPrograms`;
//! the trait exists so the store carries programs without depending on the
//! expression layer.
//!
//! [`scan_view`](crate::InstanceStore::scan_view) evaluates a [`ViewSource`]
//! through a program in Rust — the oracle — while a pushdown backend ships the
//! `*_wire` faces into SQL. Parity is by construction: same faces, same
//! interpreter — only where the candidate rows come from differs.

use core::cmp::Ordering;

use liasse_ident::{NameSegment, RowIncarnation};
use liasse_value::Value;

use crate::contract::InstanceStore;
use crate::error::StoreError;
use crate::key::{AddressStep, CollectionPath, KeyValue, RowAddress};

/// The depth beyond which a live subtree descent (coverage or prefetch) is a
/// corruption bail rather than a hang or a truncation. A well-formed store's data
/// is finite-depth; only cyclic or corrupt over-deep data trips this, on both the
/// in-memory oracle and a pushdown backend, as the same corruption-classed error.
pub const MAX_SUBTREE_DEPTH: usize = 4096;

/// The direction and priority of a lowered view's `$sort` keys (§7.3): one entry
/// per sort key, highest priority first. The store orders [`EvaluatedRow`]s by the
/// evaluated sort tuple under these directions, with the key path as the final
/// occurrence tiebreak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    /// Present values ascending, then `none` (B.2).
    Ascending,
    /// `none` first, then present values descending — the reversal of ascending.
    Descending,
}

/// An evaluation fault raised by a [`ViewProgram`] face: a division by zero on the
/// candidate's values, an unbound reference the residual should never have carried,
/// a shape mismatch. Reported through [`StoreError::Eval`], which the runtime
/// answers with the interpreter fallback so the surfaced behaviour is
/// interpreter-exact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalFault {
    detail: String,
}

impl EvalFault {
    /// A fault carrying a human-readable reason.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self { detail: detail.into() }
    }

    /// The reason.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// The candidate's live subtree, prefetched by the store when the program's
/// read-set ([`ViewProgram::subtree_steps`]) is non-empty: every live row strictly
/// under the candidate through the read-set's nested collection names — each with
/// its relative path from the candidate (one `(step, key)` component per descended
/// level) and its stored value, in Annex B address order. Live rows only: a
/// tombstone blocks its branch. Empty for the common shallow program.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateSubtree(pub Vec<(Vec<(String, KeyValue)>, Value)>);

/// A compiled per-row evaluation program, opaque to the store contract.
///
/// Each face is total over `(stored payload, typed key, prefetched subtree)`; an
/// evaluation fault is an [`EvalFault`], never a silent verdict. The `*_wire`
/// methods return the version-locked serialized faces a pushdown backend ships to
/// its in-database twin; the in-memory oracle never calls them.
pub trait ViewProgram: Send + Sync {
    /// The nested-collection step names the program reads through the candidate —
    /// the compiler-extracted candidate-subtree read-set. Empty for the common
    /// shallow program; non-empty directs the store to prefetch each candidate's
    /// live subtree over exactly these steps before calling the faces.
    fn subtree_steps(&self) -> &[String];

    /// The admit verdict over one row. For a flat view this is the lowered filter;
    /// for §10.5 coverage it is the composed hereditary `$where && !$except`
    /// (§7.2). Truthiness is strict `Bool(true)`. A program with no filter admits
    /// everything.
    fn admits(
        &self,
        value: &Value,
        key: &KeyValue,
        subtree: &CandidateSubtree,
    ) -> Result<bool, EvalFault>;

    /// The projected output row: the scalar/struct output cells of the view
    /// projection with computed fields folded, as one `Value::Struct` in
    /// output-name order (a `none` output an omitted member). Keyed sub-view cells
    /// are not part of this scalar projection.
    fn project(
        &self,
        value: &Value,
        key: &KeyValue,
        subtree: &CandidateSubtree,
    ) -> Result<Value, EvalFault>;

    /// The evaluated `$sort` tuple (§7.3), highest priority first; empty for an
    /// unsorted view (order = source key order).
    fn sort_tuple(
        &self,
        value: &Value,
        key: &KeyValue,
        subtree: &CandidateSubtree,
    ) -> Result<Vec<Value>, EvalFault>;

    /// The per-key sort directions, aligned with [`ViewProgram::sort_tuple`]. The
    /// store orders rows by the tuple under these directions, then the key path.
    fn sort_directions(&self) -> &[SortDirection];

    /// The version-locked serialized admit face a pushdown backend ships to its
    /// in-database twin, or `None` when the program has no filter. The oracle never
    /// calls this.
    fn admit_wire(&self) -> Option<&[u8]>;

    /// The version-locked serialized projection face. The oracle never calls this.
    fn project_wire(&self) -> &[u8];

    /// The version-locked serialized sort face, or `None` for an unsorted view. The
    /// oracle never calls this.
    fn sort_wire(&self) -> Option<&[u8]>;

    /// The version-locked serialized hoisted-env wire, shared by every face of one
    /// lowered view. The oracle never calls this.
    fn env_wire(&self) -> &[u8];
}

/// Where the evaluated read draws its candidate rows.
pub enum ViewSource<'a> {
    /// A collection's direct rows (§4.2's scan, evaluated): the common `$view`.
    Collection(&'a CollectionPath),
    /// The §10.5 coverage tree under `root` through nested keyed collection
    /// `field`: depth-first in Annex B key order, live rows only (a tombstone
    /// blocks its branch), each DESCENDANT admitted hereditarily by
    /// [`ViewProgram::admits`]. The root row itself is NOT filtered — the covered
    /// row is admitted by scope membership, which the caller has already resolved.
    /// The root IS projected.
    Coverage { root: &'a RowAddress, field: &'a str },
}

/// One evaluated result row: its relative key path from the source (one component
/// for a flat scan, one per descended level for coverage — empty for the coverage
/// root), its incarnation, the projected output struct, and the evaluated sort
/// tuple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedRow {
    /// The relative key path from the source.
    pub key_path: Vec<KeyValue>,
    /// The row's durable incarnation (D.1).
    pub incarnation: RowIncarnation,
    /// The projected output struct.
    pub projected: Value,
    /// The evaluated `$sort` tuple (empty for an unsorted view or coverage).
    pub sort: Vec<Value>,
}

/// Evaluate `source`'s rows through `program`, in the view's delivered order — the
/// Annex-B sort-tuple order (under the program's directions) with the key path as
/// the final occurrence tiebreak when the program sorts, else source order (flat:
/// key order; coverage: depth-first key order). `skip`/`limit` apply after ordering
/// for a [`ViewSource::Collection`]; [`ViewSource::Coverage`] ignores them.
///
/// The default oracle over any store's `scan`/`row` primitives. A pushdown backend
/// overrides this with one SQL statement.
pub(crate) fn scan_view_impl<S: InstanceStore + ?Sized>(
    store: &S,
    source: ViewSource<'_>,
    program: &dyn ViewProgram,
    skip: Option<u64>,
    limit: Option<u64>,
) -> Result<Vec<EvaluatedRow>, StoreError> {
    match source {
        ViewSource::Collection(path) => scan_collection(store, path, program, skip, limit),
        ViewSource::Coverage { root, field } => scan_coverage(store, root, field, program),
    }
}

/// The flat `$view` path: admit, project, and sort-evaluate a collection's direct
/// rows, then order and bound them.
fn scan_collection<S: InstanceStore + ?Sized>(
    store: &S,
    path: &CollectionPath,
    program: &dyn ViewProgram,
    skip: Option<u64>,
    limit: Option<u64>,
) -> Result<Vec<EvaluatedRow>, StoreError> {
    let mut evaluated = Vec::new();
    for (address, stored) in store.scan(path)? {
        let key = last_key(&address)?;
        let subtree = prefetch_subtree(store, &address, program.subtree_steps())?;
        if !program.admits(stored.value(), key, &subtree).map_err(eval_error)? {
            continue;
        }
        let projected = program.project(stored.value(), key, &subtree).map_err(eval_error)?;
        let sort = program.sort_tuple(stored.value(), key, &subtree).map_err(eval_error)?;
        evaluated.push(EvaluatedRow {
            key_path: vec![key.clone()],
            incarnation: stored.incarnation().clone(),
            projected,
            sort,
        });
    }
    order_and_bound(&mut evaluated, program.sort_directions(), skip, limit);
    Ok(evaluated)
}

/// The §10.5 coverage path: project the root unconditionally, then descend through
/// `field` in depth-first Annex-B key order, admitting each descendant
/// hereditarily and recursing only into admitted (and live) candidates.
fn scan_coverage<S: InstanceStore + ?Sized>(
    store: &S,
    root: &RowAddress,
    field: &str,
    program: &dyn ViewProgram,
) -> Result<Vec<EvaluatedRow>, StoreError> {
    let mut out = Vec::new();
    // The root is admitted by scope membership (already resolved) and projected
    // unconditionally; predicates admit its DESCENDANTS. A tombstoned/absent root
    // yields nothing.
    let Some(root_row) = store.row(root)? else {
        return Ok(out);
    };
    let root_key = last_key(root)?;
    let root_subtree = prefetch_subtree(store, root, program.subtree_steps())?;
    out.push(EvaluatedRow {
        key_path: Vec::new(),
        incarnation: root_row.incarnation().clone(),
        projected: program.project(root_row.value(), root_key, &root_subtree).map_err(eval_error)?,
        sort: Vec::new(),
    });
    cover_descend(store, root, field, program, &mut Vec::new(), 0, &mut out)?;
    Ok(out)
}

/// Descend one level of coverage: scan the live children of the row at `parent`
/// through `field` (Annex-B key order), admit each hereditarily, project and emit
/// admitted candidates, and recurse into them. `path` is the relative key path
/// from the coverage root to `parent`.
fn cover_descend<S: InstanceStore + ?Sized>(
    store: &S,
    parent: &RowAddress,
    field: &str,
    program: &dyn ViewProgram,
    path: &mut Vec<KeyValue>,
    depth: usize,
    out: &mut Vec<EvaluatedRow>,
) -> Result<(), StoreError> {
    if depth >= MAX_SUBTREE_DEPTH {
        return Err(over_deep("coverage descent"));
    }
    let child_path = CollectionPath::nested(parent.steps().cloned(), NameSegment::new(field));
    for (address, stored) in store.scan(&child_path)? {
        let key = last_key(&address)?;
        let subtree = prefetch_subtree(store, &address, program.subtree_steps())?;
        if !program.admits(stored.value(), key, &subtree).map_err(eval_error)? {
            continue;
        }
        path.push(key.clone());
        out.push(EvaluatedRow {
            key_path: path.clone(),
            incarnation: stored.incarnation().clone(),
            projected: program.project(stored.value(), key, &subtree).map_err(eval_error)?,
            sort: Vec::new(),
        });
        cover_descend(store, &address, field, program, path, depth + 1, out)?;
        path.pop();
    }
    Ok(())
}

/// Build the candidate's live subtree over `steps` by a depth-guarded descent over
/// the store's `scan` primitive: every live row strictly under `root` reachable
/// through the read-set steps, each with its relative `(step, key)` path and value.
/// Empty (and query-free) for a shallow program.
fn prefetch_subtree<S: InstanceStore + ?Sized>(
    store: &S,
    root: &RowAddress,
    steps: &[String],
) -> Result<CandidateSubtree, StoreError> {
    if steps.is_empty() {
        return Ok(CandidateSubtree::default());
    }
    let mut rows = Vec::new();
    subtree_level(store, root, steps, &mut Vec::new(), 0, &mut rows)?;
    Ok(CandidateSubtree(rows))
}

/// One level of the subtree descent: the live children of `parent` through every
/// step in `steps`, recorded with their relative path, then recursion into each.
fn subtree_level<S: InstanceStore + ?Sized>(
    store: &S,
    parent: &RowAddress,
    steps: &[String],
    rel: &mut Vec<(String, KeyValue)>,
    depth: usize,
    out: &mut Vec<(Vec<(String, KeyValue)>, Value)>,
) -> Result<(), StoreError> {
    if depth >= MAX_SUBTREE_DEPTH {
        return Err(over_deep("candidate subtree descent"));
    }
    for step in steps {
        let path = CollectionPath::nested(parent.steps().cloned(), NameSegment::new(step.as_str()));
        for (address, stored) in store.scan(&path)? {
            let key = last_key(&address)?;
            rel.push((step.clone(), key.clone()));
            out.push((rel.clone(), stored.value().clone()));
            subtree_level(store, &address, steps, rel, depth + 1, out)?;
            rel.pop();
        }
    }
    Ok(())
}

/// Order the evaluated rows by the sort tuple under `directions`, then the key
/// path, and apply `skip`/`limit`. With no directions the rows keep source order
/// (the scan's Annex-B key order); the key-path tiebreak is a stable no-op there.
fn order_and_bound(
    rows: &mut Vec<EvaluatedRow>,
    directions: &[SortDirection],
    skip: Option<u64>,
    limit: Option<u64>,
) {
    if !directions.is_empty() {
        rows.sort_by(|a, b| compare_sorted(a, b, directions));
    }
    if let Some(skip) = skip {
        let skip = usize::try_from(skip).unwrap_or(usize::MAX).min(rows.len());
        rows.drain(..skip);
    }
    if let Some(limit) = limit {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX).min(rows.len());
        rows.truncate(limit);
    }
}

/// The delivered total order of a sorted flat view (§7.3): successive sort keys
/// with each descending key reversed, then the key path as the occurrence tiebreak
/// (Annex D.1). A missing key component compares equal, deferring to the tiebreak —
/// the same reduction the interpreter's `SortOrder::compare` makes.
fn compare_sorted(a: &EvaluatedRow, b: &EvaluatedRow, directions: &[SortDirection]) -> Ordering {
    for (index, direction) in directions.iter().enumerate() {
        let ordering = match (a.sort.get(index), b.sort.get(index)) {
            (Some(x), Some(y)) => x.cmp(y),
            _ => Ordering::Equal,
        };
        let ordering = match direction {
            SortDirection::Ascending => ordering,
            SortDirection::Descending => ordering.reverse(),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    a.key_path.cmp(&b.key_path)
}

/// The typed key of the row at `address` — its last address step's key.
fn last_key(address: &RowAddress) -> Result<&KeyValue, StoreError> {
    address
        .steps()
        .last()
        .map(AddressStep::key)
        .ok_or_else(|| StoreError::Corruption { detail: "row address has no steps".to_owned() })
}

fn eval_error(fault: EvalFault) -> StoreError {
    StoreError::Eval { detail: fault.detail }
}

fn over_deep(context: &str) -> StoreError {
    StoreError::Corruption {
        detail: format!(
            "{context} exceeded the maximum depth {MAX_SUBTREE_DEPTH} — cyclic or corrupt over-deep data"
        ),
    }
}
