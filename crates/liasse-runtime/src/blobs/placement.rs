//! The §18.4 placement-policy plan and the §18.5 logical placement observations.
//!
//! A [`Placement`] is the `$in` plan over store identities that a new write must
//! fulfill and that existing verified copies are measured against. The engine
//! side (`super`) turns a plan into a writable set and lands copies; the pure
//! logic here answers the §18.5 observations off a set of *verified* stores,
//! independent of any connector:
//!
//! - `$stored` — the verified stores (the caller reads them off the [`Blob`]);
//! - `$satisfied` — [`Placement::satisfied_by`], the policy evaluated over the
//!   verified set (existing copies satisfy the policy when any branch is,
//!   §18.4);
//! - `$surplus` — [`Placement::surplus`], the verified copies outside the
//!   *currently required* policy (the drain candidates of §18.6 step 5).
//!
//! `$satisfied`/`$surplus` are evaluated against the *current* resolution of the
//! policy, not the admission-time one: disabling a store shrinks the store view
//! the policy resolves to, so an already-verified copy in a no-longer-required
//! store becomes surplus without any bytes moving (§18.5).

use std::collections::BTreeSet;

/// A store identity (`stores.id`, §18.3).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StoreId(String);

impl StoreId {
    /// Wrap a store id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The store-id text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A store row (§18.3): its id, the connector it selects, and whether it is
/// enabled for placement.
#[derive(Debug, Clone)]
pub struct Store {
    /// The store id.
    pub id: StoreId,
    /// The registered connector name this store selects.
    pub connector: String,
    /// Whether the store participates in placement (`enabled`).
    pub enabled: bool,
}

/// A placement policy plan (§18.4). A bare `view` requires every store it
/// yields; the branch combinators compose those requirements.
#[derive(Debug, Clone)]
pub enum Placement {
    /// A store view: verified in every store it yields.
    View(Vec<StoreId>),
    /// Every branch required simultaneously.
    All(Vec<Placement>),
    /// Alternatives in preference order; the first fulfillable is the new-write
    /// plan.
    Any(Vec<Placement>),
    /// Any `n` writable stores from the ordered view.
    Copies {
        /// The required copy count.
        n: usize,
        /// The ordered source view.
        of: Vec<StoreId>,
    },
}

impl Placement {
    /// The flattened depth-first, left-to-right store order with duplicate store
    /// identities removed by first occurrence (§18.4). This is the default
    /// `$serve` order.
    #[must_use]
    pub fn flattened(&self) -> Vec<StoreId> {
        let mut seen = BTreeSet::new();
        let mut order = Vec::new();
        self.collect(&mut seen, &mut order);
        order
    }

    fn collect(&self, seen: &mut BTreeSet<StoreId>, order: &mut Vec<StoreId>) {
        match self {
            Self::View(stores) | Self::Copies { of: stores, .. } => {
                for store in stores {
                    if seen.insert(store.clone()) {
                        order.push(store.clone());
                    }
                }
            }
            Self::All(branches) | Self::Any(branches) => {
                for branch in branches {
                    branch.collect(seen, order);
                }
            }
        }
    }

    /// `blob.$satisfied` (§18.5): whether the `verified` store set satisfies this
    /// policy. Existing verified copies satisfy the policy when any branch is
    /// satisfied (§18.4): a `view` needs every store it yields, `$all` needs
    /// every branch, `$any` needs one branch, and `$copies` needs `n` distinct
    /// verified stores from its source view.
    #[must_use]
    pub fn satisfied_by(&self, verified: &BTreeSet<StoreId>) -> bool {
        match self {
            Self::View(stores) => dedup(stores).iter().all(|s| verified.contains(s)),
            Self::All(branches) => branches.iter().all(|b| b.satisfied_by(verified)),
            Self::Any(branches) => branches.iter().any(|b| b.satisfied_by(verified)),
            Self::Copies { n, of } => {
                dedup(of).iter().filter(|s| verified.contains(*s)).count() >= *n
            }
        }
    }

    /// `blob.$surplus` (§18.5): the `verified` copies outside the currently
    /// required policy — the verified complement of [`required_for`]. Draining a
    /// surplus copy is grace-gated (§18.6 step 5); the observation itself is
    /// immediate.
    ///
    /// [`required_for`]: Placement::required_for
    #[must_use]
    pub fn surplus(&self, verified: &BTreeSet<StoreId>) -> Vec<StoreId> {
        let required = self.required_for(verified);
        verified.iter().filter(|s| !required.contains(*s)).cloned().collect()
    }

    /// The verified stores that participate in satisfying this policy — the
    /// "currently required policy" of §18.5. A `view` requires every store it
    /// yields; `$all` unions its branches; `$any` takes the first satisfied
    /// branch (none satisfied ⇒ nothing is required, so every verified copy is
    /// surplus); `$copies` takes the first `n` verified stores in declared
    /// order, so a verified copy beyond `n` is surplus.
    fn required_for(&self, verified: &BTreeSet<StoreId>) -> BTreeSet<StoreId> {
        match self {
            Self::View(stores) => dedup(stores).into_iter().collect(),
            Self::All(branches) => {
                branches.iter().flat_map(|b| b.required_for(verified)).collect()
            }
            Self::Any(branches) => branches
                .iter()
                .find(|b| b.satisfied_by(verified))
                .map(|b| b.required_for(verified))
                .unwrap_or_default(),
            Self::Copies { n, of } => {
                dedup(of).into_iter().filter(|s| verified.contains(s)).take(*n).collect()
            }
        }
    }
}

/// The lifecycle state of one placement copy (§18.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyState {
    /// Staged, not yet copying.
    Pending,
    /// Copy in progress.
    Copying,
    /// Verified at its destination.
    Verified,
    /// Observed to hash wrong; demoted for repair.
    Corrupt,
    /// Being drained as surplus.
    Draining,
}

/// The §18.5 logical placement observations of a committed blob occurrence: the
/// verified stores (`$stored`), whether the current policy is satisfied over
/// them (`$satisfied`), and the verified copies outside the currently required
/// policy (`$surplus`). These are the engine-recorded observations §18.5 exposes
/// off a descriptor occurrence; the implementation form is internal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementState {
    /// `blob.$stored`: the verified stores holding this content.
    pub stored: Vec<StoreId>,
    /// `blob.$satisfied`: the placement policy evaluated over `$stored`.
    pub satisfied: bool,
    /// `blob.$surplus`: verified copies outside the currently required policy.
    pub surplus: Vec<StoreId>,
}

/// Remove repeated store identities by first occurrence (§18.4).
pub(crate) fn dedup(stores: &[StoreId]) -> Vec<StoreId> {
    let mut seen = BTreeSet::new();
    stores.iter().filter(|s| seen.insert((*s).clone())).cloned().collect()
}
