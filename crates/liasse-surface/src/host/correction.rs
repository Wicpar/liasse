//! Host correction of a §19.9 reconciliation plan, addressed by D.3 display path.
//!
//! §19.9: a failed automatic merge returns a reconciliation plan; a host
//! correction function "may select or provide valid values and resolve direct
//! child-mount choices within each affected boundary", after which Liasse
//! validates and activates the complete prospective composition. This module is
//! the surface's part of that: it takes the plan's conflicted coordinates and a
//! host *choose map* keyed by display path, and resolves which side each conflict
//! takes.
//!
//! The addressing is the point (§D.3). A display path alternates declaration-name
//! segments and canonical key-text segments, and "Key segments use the
//! scalar-component encoding above before composite joining." So a row keyed with
//! the text `a/b` addresses as `/notes/a%2Fb/body`: the escaped `%2F` keeps the
//! `/` inside the key from being confused with a path separator. A correction that
//! matched on raw text would misparse `/notes/a/b/body` as a nested `b` under `a`
//! (or fail to address the row); resolving through the D.3 codec defends exactly
//! that attack.
//!
//! ## Runtime seam
//!
//! Two runtime seams remain. First, the surface takes structured
//! [`ConflictCoordinate`]s rather than the [`MergeOutcome`](liasse_runtime::MergeOutcome)
//! `conflicts` directly, because a [`MergeConflict`](liasse_runtime::MergeConflict)
//! carries only a diagnostic `coordinate` string (`RowAddress::render`, JSON-quoted
//! key text — explicitly *not* the D.3 display path), from which the escaped D.3
//! path cannot be recovered unambiguously. Second, §19.9 *activation* (committing
//! the corrected composition into a new lineage that preserves both source
//! histories) needs an engine primitive that installs a computed merged state; the
//! engine exposes none (`call`/`import`/`update` are the only commit paths), so
//! this verb resolves the correction but does not yet commit it.

use std::collections::{BTreeMap, BTreeSet};

use liasse_ident::{CanonicalPath, IdentError, KeyText, NameSegment, PathSegment};
use liasse_store::InstanceStore;
use liasse_value::Value;

use super::SurfaceHost;

/// Which side of a conflicted coordinate a host correction selects (§19.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChooseSide {
    /// Take the incoming side's value at this coordinate.
    Incoming,
    /// Keep the local side's value at this coordinate.
    Local,
}

/// One conflicted logical coordinate in a reconciliation plan (§19.9): the
/// collection, the row key, and the conflicted field (absent for a whole-row
/// delete-vs-modify or competing-insert conflict).
///
/// It renders its own D.3 display path (§D.3) so a host correction addresses it
/// unambiguously even when the key contains the path separator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictCoordinate {
    collection: String,
    key: Value,
    field: Option<String>,
}

impl ConflictCoordinate {
    /// A conflict at `field` of the `collection` row keyed `key` (§19.9 field
    /// conflict).
    #[must_use]
    pub fn field(collection: impl Into<String>, key: Value, field: impl Into<String>) -> Self {
        Self { collection: collection.into(), key, field: Some(field.into()) }
    }

    /// A whole-row conflict on the `collection` row keyed `key` (§19.9
    /// delete-vs-modify / competing insert).
    #[must_use]
    pub fn row(collection: impl Into<String>, key: Value) -> Self {
        Self { collection: collection.into(), key, field: None }
    }

    /// The canonical D.3 display path of this coordinate: the collection name, the
    /// key rendered as a canonical key-text segment (each scalar component escaped
    /// before any composite `:` join, §D.2), and the field name when present. The
    /// key escaping is what makes `/notes/a%2Fb/body` address the `a/b` row rather
    /// than a nested path (§D.3).
    ///
    /// # Errors
    /// [`IdentError`] when the key holds a value D.2 gives no key text (a `json`,
    /// `blob`, `set`, `map`, or `none`).
    pub fn display_path(&self) -> Result<String, IdentError> {
        let key_text = KeyText::from_key_values(std::slice::from_ref(&self.key))?;
        let mut segments = vec![
            PathSegment::Name(NameSegment::new(self.collection.clone())),
            PathSegment::Key(key_text),
        ];
        if let Some(field) = &self.field {
            segments.push(PathSegment::Name(NameSegment::new(field.clone())));
        }
        Ok(CanonicalPath::new(segments).to_display_string())
    }
}

/// A host correction's choose map (§19.9): one decision per conflicted coordinate,
/// keyed by the coordinate's D.3 display path exactly as it renders.
#[derive(Debug, Clone, Default)]
pub struct ChooseMap {
    choices: BTreeMap<String, ChooseSide>,
}

impl ChooseMap {
    /// An empty choose map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Choose `side` for the coordinate at display `path`.
    #[must_use]
    pub fn with(mut self, path: impl Into<String>, side: ChooseSide) -> Self {
        self.choices.insert(path.into(), side);
        self
    }

    /// The side chosen for display `path`, if any.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<ChooseSide> {
        self.choices.get(path).copied()
    }

    /// The display paths this correction addresses.
    fn paths(&self) -> impl Iterator<Item = &String> {
        self.choices.keys()
    }
}

/// The resolved outcome of applying a host correction (§19.9): the side accepted
/// at each conflicted coordinate, keyed by D.3 display path, and whether every
/// conflict in the plan was resolved (a complete, activation-ready correction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionOutcome {
    resolved: BTreeMap<String, ChooseSide>,
    complete: bool,
}

impl CorrectionOutcome {
    /// The side accepted at display `path`, if the correction resolved it.
    #[must_use]
    pub fn chosen(&self, path: &str) -> Option<ChooseSide> {
        self.resolved.get(path).copied()
    }

    /// Whether every conflict in the plan was resolved. A complete correction is
    /// the prospective composition §19.9 would validate and activate.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// The accepted side per display path.
    #[must_use]
    pub fn resolved(&self) -> &BTreeMap<String, ChooseSide> {
        &self.resolved
    }
}

/// Why a host correction could not be resolved against its plan (§19.9).
#[derive(Debug, thiserror::Error)]
pub enum CorrectionError {
    /// A conflicted coordinate has no D.3 display path (a non-key-eligible key).
    #[error("conflict coordinate in `{collection}` has no D.3 display path: {source}")]
    Coordinate {
        /// The collection the coordinate names.
        collection: String,
        /// The underlying D.2/D.3 encoding fault.
        source: IdentError,
    },
    /// A conflict in the plan was left unresolved by the choose map (§19.9
    /// requires a complete correction before activation).
    #[error("conflict at `{0}` was left unresolved by the correction")]
    Unresolved(String),
    /// A choose entry addresses a display path that is no conflict in the plan —
    /// a stray or misspelled path (including a raw, unescaped key that never
    /// matches the escaped D.3 coordinate, §D.3).
    #[error("correction chooses `{0}`, which addresses no conflict in the plan")]
    Unmatched(String),
}

impl<S: InstanceStore> SurfaceHost<S> {
    /// Apply a host correction over a §19.9 reconciliation plan's conflicts,
    /// addressed by D.3 display path.
    ///
    /// Each conflict must be chosen exactly once and every choose key must address
    /// a real conflict. Matching is by the coordinate's escaped D.3 display path,
    /// so a choose key `/notes/a%2Fb/body` resolves the `a/b` row's body and never
    /// a spurious nested `b` under `a`; a raw `/notes/a/b/body` matches no
    /// coordinate and is [`CorrectionError::Unmatched`] (§D.3).
    ///
    /// The returned outcome is the accepted correction. Committing it into a new
    /// lineage (§19.9 activation) is a runtime seam the surface cannot yet drive
    /// (see the module docs).
    ///
    /// # Errors
    /// [`CorrectionError`] when a coordinate has no D.3 path, a conflict is left
    /// unresolved, or a choose key addresses no conflict.
    pub fn apply_correction(
        &self,
        conflicts: &[ConflictCoordinate],
        choose: &ChooseMap,
    ) -> Result<CorrectionOutcome, CorrectionError> {
        let mut resolved = BTreeMap::new();
        let mut matched = BTreeSet::new();
        for conflict in conflicts {
            let path = conflict.display_path().map_err(|source| CorrectionError::Coordinate {
                collection: conflict.collection.clone(),
                source,
            })?;
            let Some(side) = choose.get(&path) else {
                return Err(CorrectionError::Unresolved(path));
            };
            resolved.insert(path.clone(), side);
            matched.insert(path);
        }
        for path in choose.paths() {
            if !matched.contains(path) {
                return Err(CorrectionError::Unmatched(path.clone()));
            }
        }
        Ok(CorrectionOutcome { resolved, complete: true })
    }
}
