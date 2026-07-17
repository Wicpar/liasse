//! Lineage-aware history-point identity (§19.2, §19.3, §19.8).
//!
//! A history point identifies one exact retained state of one package instance
//! and is a *pair* — a lineage and a position within it (§19.3). The store's
//! commit seat cannot be that identity: a restore restarts the seat at the
//! genesis position, and an applied import stages its movement as a fresh commit,
//! so raw seat integers collide across a restore boundary and shift under a
//! rollback. A continuation re-exported from a restored sandbox would reuse the
//! base's seat numbers, and a rollback+re-export would move the seat forward while
//! the logical point moved back.
//!
//! [`HistoryCursor`] is the engine-owned logical position that decouples point
//! identity from the volatile store seat. It advances by one position per
//! state-changing commit, is preserved verbatim across export/restore (§19.2
//! "once a history point has been exported, its identity and relative position
//! remain stable"), and carries the lineage ancestry a rollback branches, so
//! [`HistoryCursor::classify`] compares an incoming point to local retained
//! history by their *lineage relationship* rather than by raw seat order (§19.8).
//!
//! ## Scope
//!
//! CORE tracks the active lineage plus the ancestor lineages the local side
//! branched through (each rollback that is continued extends a new lineage from
//! the selected point, §19.3). The displaced continuation a rollback leaves
//! behind is retained by the artifact container as an alternate lineage head; the
//! cursor records the branch point, not the displaced head, so a
//! `same-lineage-then-divergence` incoming still classifies as a merge. Foreign
//! point-id aliasing across genuinely unrelated histories stays unpinned
//! (SPEC-ISSUES item 21): an incoming lineage the local ancestry does not know is
//! reported `unrelated`.

use liasse_ident::{HistoryPoint, InstanceId, LineageId, PointId};
use serde_json::Value as J;

use crate::history::ImportRelation;

/// One lineage in an instance's local ancestry: its identifier and the point
/// position on its parent lineage it branched from (`None` for the lineage that
/// starts at genesis, §19.6).
#[derive(Debug, Clone)]
struct Link {
    lineage: LineageId,
    origin: Option<u64>,
}

/// The engine's logical position in its own history (§19.2/§19.3): the active
/// lineage, the selected point within it, the ancestor lineages it descends
/// through, and the monotone allocator that keeps every point position distinct.
///
/// A point's identity is `(active lineage, point)` with the point rendered as its
/// decimal position; positions are never reused, so two points are the same point
/// exactly when their lineage and position agree.
#[derive(Debug, Clone)]
pub(crate) struct HistoryCursor {
    /// Proper ancestor lineages, root-first; the active lineage descends from the
    /// last of these (or from genesis when empty).
    ancestors: Vec<Link>,
    /// The active lineage the current point belongs to.
    active: Link,
    /// The current selected point's position within the active lineage.
    point: u64,
    /// The next fresh position to allocate — monotone across the whole instance
    /// history, so a continuation and a branch never reuse a position.
    next: u64,
    /// Whether the current point already carries a displaced continuation, so the
    /// next commit branches a new lineage (§19.3 "Continuing from the selected
    /// point extends a new lineage"). Set by a rollback.
    branch_pending: bool,
}

impl HistoryCursor {
    /// The genesis cursor of `instance` (§19.3): its genesis lineage at the first
    /// point. The genesis commit is point 1; the first mutation advances to 2.
    pub(crate) fn genesis(instance: &InstanceId) -> Self {
        Self {
            ancestors: Vec::new(),
            active: Link { lineage: genesis_lineage(instance), origin: None },
            point: GENESIS_POINT,
            next: GENESIS_POINT + 1,
            branch_pending: false,
        }
    }

    /// The cursor a restore adopts from an artifact's `selected` point and its
    /// `history/index.json` (§19.2/§19.10): the selected lineage and point become
    /// the current position and the index's origin chain becomes the ancestry, so
    /// a re-export reproduces the exact selected point and a later classify is
    /// lineage-aware. A malformed or absent index falls back to a single
    /// genesis-rooted lineage at the selected point.
    pub(crate) fn restored(selected: &HistoryPoint, index: &[u8]) -> Self {
        let point = position(selected.point()).unwrap_or(GENESIS_POINT);
        let mut chain = restored_chain(selected.lineage(), index);
        // The chain is root-first with the selected lineage last; a non-empty
        // result is guaranteed by `restored_chain`, but keep the split total.
        let active = chain.pop().unwrap_or(Link { lineage: selected.lineage().clone(), origin: None });
        Self {
            ancestors: chain,
            active,
            point,
            next: point + 1,
            branch_pending: false,
        }
    }

    /// The current selected history point `(lineage, point)` (§19.3), the identity
    /// an export names and an import classifies against.
    pub(crate) fn point(&self) -> HistoryPoint {
        HistoryPoint::new(self.active.lineage.clone(), PointId::new(self.point.to_string()))
    }

    /// Advance to a fresh point on the active lineage, the identity a
    /// state-changing commit takes (§19.2). When a rollback left the current point
    /// displaced, this first branches a new lineage from it (§19.3).
    pub(crate) fn advance(&mut self) {
        if self.branch_pending {
            self.branch(branch_lineage(&self.active.lineage, self.next));
        }
        self.point = self.next;
        self.next += 1;
    }

    /// Record a §19.9 reconciliation: branch a new lineage from the current point
    /// and advance onto it, so a re-export names the reconciled result on its own
    /// lineage over the prior history (§19.9 "records the accepted correction in a
    /// new lineage").
    pub(crate) fn begin_reconciled(&mut self) {
        self.branch(reconciled_lineage(&self.active.lineage, self.next));
        self.point = self.next;
        self.next += 1;
    }

    /// Move to the `incoming` point on the active lineage, the identity an applied
    /// fast-forward adopts (§19.8): the local point precedes the incoming, so the
    /// incoming continuation becomes the current point on the same lineage. A
    /// no-op when the incoming point carries no decimal position (a movement is
    /// only permitted after a classification that parsed it, so this cannot occur).
    pub(crate) fn apply_fast_forward(&mut self, incoming: &HistoryPoint) {
        if let Some(incoming) = position(incoming.point()) {
            self.point = incoming;
            self.next = self.next.max(incoming + 1);
            self.branch_pending = false;
        }
    }

    /// Select the earlier `incoming` point, the identity an applied rollback
    /// adopts (§19.8/§19.3): the current continuation is displaced and the next
    /// commit branches a new lineage. The incoming point is on the active lineage
    /// or an ancestor of it (the classifier established it precedes local before
    /// the movement was permitted).
    pub(crate) fn apply_rollback(&mut self, incoming: &HistoryPoint) {
        let Some(position) = position(incoming.point()) else { return };
        let target = incoming.lineage();
        if *target != self.active.lineage
            && let Some(i) = self.ancestors.iter().position(|link| link.lineage == *target)
            && let Some(link) = self.ancestors.get(i).cloned()
        {
            self.ancestors.truncate(i);
            self.active = link;
        }
        self.point = position;
        self.branch_pending = true;
    }

    /// Classify an incoming point against local retained history by its lineage
    /// relationship (§19.8): the identical point is `SamePoint`; a point the local
    /// side descends from is behind, so a `Rollback` is available; a point that
    /// continues the local lineage is ahead, so a `FastForward` is available; a
    /// point on an ancestor lineage past the branch shares an ancestor then
    /// diverges, so a `Merge` is required; a lineage the local ancestry does not
    /// know shares no point, so an `Unrelated` policy governs.
    pub(crate) fn classify(&self, incoming: &HistoryPoint) -> ImportRelation {
        let Some(incoming_point) = position(incoming.point()) else {
            // A same-instance point whose position is not a decimal is a foreign
            // identifier aliased onto local history (SPEC-ISSUES item 21): no
            // shared point is knowable.
            return ImportRelation::Unrelated;
        };
        let incoming_lineage = incoming.lineage();
        if *incoming_lineage == self.active.lineage {
            return match incoming_point.cmp(&self.point) {
                std::cmp::Ordering::Equal => ImportRelation::SamePoint,
                std::cmp::Ordering::Less => ImportRelation::Rollback,
                std::cmp::Ordering::Greater => ImportRelation::FastForward,
            };
        }
        match self.ancestors.iter().position(|link| link.lineage == *incoming_lineage) {
            Some(i) => {
                // The branch point on this ancestor is the origin of the lineage
                // that descends from it (the next chain link, or the active
                // lineage for the last ancestor).
                let branch_at = self
                    .ancestors
                    .get(i + 1)
                    .map_or(self.active.origin, |next| next.origin)
                    .unwrap_or(self.point);
                if incoming_point <= branch_at {
                    // At or before the shared branch point: the incoming precedes
                    // local — a rollback target.
                    ImportRelation::Rollback
                } else {
                    // Past the branch point on the ancestor lineage: a shared
                    // point followed by divergence — a three-way merge.
                    ImportRelation::Merge
                }
            }
            None => ImportRelation::Unrelated,
        }
    }

    /// The retained lineages, root-first, for the `history/index.json` the export
    /// writes (§19.6): each carries its origin (`None` = genesis, `Some(parent,
    /// position)` = a branch) and the head position the local side knows. An
    /// ancestor's head is the point its descendant branched off; the active
    /// lineage's head is the current point.
    pub(crate) fn lineages(&self) -> Vec<LineageEntry> {
        let mut out = Vec::with_capacity(self.ancestors.len() + 1);
        let mut parent: Option<&LineageId> = None;
        for (i, link) in self.ancestors.iter().enumerate() {
            let head = self
                .ancestors
                .get(i + 1)
                .map_or(self.active.origin, |next| next.origin)
                .unwrap_or(self.point);
            out.push(LineageEntry::new(link, parent, head));
            parent = Some(&link.lineage);
        }
        out.push(LineageEntry::new(&self.active, parent, self.point));
        out
    }

    /// Push the active lineage down as an ancestor and continue on `new_lineage`
    /// branched from the current point.
    fn branch(&mut self, new_lineage: LineageId) {
        let origin = self.point;
        self.ancestors.push(self.active.clone());
        self.active = Link { lineage: new_lineage, origin: Some(origin) };
        self.branch_pending = false;
    }
}

/// One lineage entry a `history/index.json` records (§19.6): its identifier, its
/// origin (the parent lineage and branch point, or genesis), and its head point.
pub(crate) struct LineageEntry {
    pub(crate) lineage: LineageId,
    pub(crate) origin: Option<(LineageId, u64)>,
    pub(crate) head: u64,
}

impl LineageEntry {
    fn new(link: &Link, parent: Option<&LineageId>, head: u64) -> Self {
        let origin = match (link.origin, parent) {
            (Some(position), Some(parent)) => Some((parent.clone(), position)),
            _ => None,
        };
        Self { lineage: link.lineage.clone(), origin, head }
    }
}

/// The genesis point position — the first retained point of an instance.
const GENESIS_POINT: u64 = 1;

/// The genesis lineage identifier of an instance (§19.3, D.5): deterministically
/// derived from the instance incarnation so a restore reconstructs the same
/// lineage identity.
fn genesis_lineage(instance: &InstanceId) -> LineageId {
    LineageId::new(format!("{}#L0", instance.as_str()))
}

/// A rollback-continuation lineage identifier (§19.3): a fresh lineage derived
/// from the one it branched off and the allocating position, so two continuations
/// from the same point never collide.
fn branch_lineage(prior: &LineageId, seat: u64) -> LineageId {
    LineageId::new(format!("{}#b{seat}", prior.as_str()))
}

/// A §19.9 reconciliation lineage identifier: a fresh lineage derived from the
/// prior one and the allocating position, so the reconciled point is
/// distinguishable from the linear history it replaced.
fn reconciled_lineage(prior: &LineageId, seat: u64) -> LineageId {
    LineageId::new(format!("{}#m{seat}", prior.as_str()))
}

/// The decimal position a point identifier encodes, or `None` when it is not a
/// runtime-minted decimal position (a foreign or opaque identifier).
fn position(point: &PointId) -> Option<u64> {
    point.as_str().parse::<u64>().ok()
}

/// Reconstruct the ancestry chain (root-first, the selected lineage last) of
/// `selected` by following the `origin` links recorded in a `history/index.json`.
/// A missing/malformed index, or a lineage without a known origin, terminates the
/// walk at a genesis-rooted lineage.
fn restored_chain(selected: &LineageId, index: &[u8]) -> Vec<Link> {
    let lineages = serde_json::from_slice::<J>(index)
        .ok()
        .and_then(|value| value.get("lineages").cloned())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();

    let mut chain: Vec<Link> = Vec::new();
    let mut current = selected.clone();
    // Bound the walk by the number of recorded lineages so a malformed cyclic
    // origin cannot loop forever.
    for _ in 0..=lineages.len() {
        let origin = lineages
            .get(current.as_str())
            .and_then(|entry| entry.get("origin"))
            .and_then(branch_origin);
        match origin {
            Some((parent, position)) => {
                chain.push(Link { lineage: current, origin: Some(position) });
                current = parent;
            }
            None => {
                chain.push(Link { lineage: current, origin: None });
                chain.reverse();
                return chain;
            }
        }
    }
    // The walk exceeded the recorded lineage count (a malformed chain): root the
    // deepest link at genesis and return what was gathered.
    chain.reverse();
    chain
}

/// Read a `{ "lineage": ..., "point": ... }` branch origin, or `None` for the
/// `"genesis"` string, an absent member, or a malformed shape.
fn branch_origin(origin: &J) -> Option<(LineageId, u64)> {
    let object = origin.as_object()?;
    let parent = object.get("lineage")?.as_str()?;
    let point = object.get("point")?.as_str()?.parse::<u64>().ok()?;
    Some((LineageId::new(parent), point))
}
