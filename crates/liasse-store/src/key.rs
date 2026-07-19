//! Typed storage keys and addresses (§5, Annex B, Annex D).
//!
//! A row is addressed structurally, not by a text string: the store never sees
//! the schema, so it cannot decode canonical key *text* back into typed key
//! values. Instead the runtime, which owns the schema, hands the store typed
//! keys directly. This keeps the store semantics-free while still ordering rows
//! exactly per Annex B — the ordering lives in [`liasse_value::Value`]'s [`Ord`].
//!
//! - [`KeyValue`] — one collection level's typed `$key`: a single scalar, or the
//!   composite components in `$key` order. Ordered lexicographically (B.4).
//! - [`AddressStep`] — a collection name plus the row's key at that level.
//! - [`RowAddress`] — the full path from the tree root to one row. Its [`Ord`]
//!   makes every collection's rows a contiguous, key-ascending range (B.5).
//! - [`CollectionPath`] — an address minus its final key: the thing a scan
//!   enumerates.

use core::cmp::Ordering;

use liasse_ident::NameSegment;
use liasse_value::Value;

use crate::error::StoreError;

/// The typed key of one row within its collection (Annex D.2 / B.4).
///
/// Held as typed [`Value`] components in `$key` order, never the D.2 colon-joined
/// text — the store orders and compares by value, and only the schema-owning
/// runtime renders text. A single-field key has one component; a composite key
/// has several. The `first`/`rest` split makes an empty key unrepresentable
/// (§5.4: a key names at least one field).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyValue {
    first: Value,
    rest: Vec<Value>,
}

impl KeyValue {
    /// A single-field key.
    #[must_use]
    pub fn single(value: Value) -> Self {
        Self { first: value, rest: Vec::new() }
    }

    /// A composite key from its components in `$key` order. The `first`/`rest`
    /// split keeps a zero-component key unrepresentable.
    #[must_use]
    pub fn composite(first: Value, rest: impl IntoIterator<Item = Value>) -> Self {
        Self { first, rest: rest.into_iter().collect() }
    }

    /// The key components in `$key` order, first to last.
    pub fn components(&self) -> impl Iterator<Item = &Value> {
        core::iter::once(&self.first).chain(self.rest.iter())
    }

    /// A colon-joined canonical-JSON rendering for diagnostics only. This is not
    /// the D.2 export text (which the schema-owning runtime produces); it exists
    /// so a [`StoreError`] can name an address.
    fn render(&self) -> String {
        let mut out = self.first.to_canonical_json_string();
        for component in &self.rest {
            out.push(':');
            out.push_str(&component.to_canonical_json_string());
        }
        out
    }
}

/// One level of a row's address: the collection declaration name and the row's
/// key at that level. Ordered by name then key, so that within a fixed parent
/// every collection's rows form one contiguous, key-ascending block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressStep {
    name: NameSegment,
    key: KeyValue,
}

impl AddressStep {
    /// Bind a collection name to a row key.
    #[must_use]
    pub fn new(name: NameSegment, key: KeyValue) -> Self {
        Self { name, key }
    }

    /// The collection declaration name.
    #[must_use]
    pub fn name(&self) -> &NameSegment {
        &self.name
    }

    /// The row key at this level.
    #[must_use]
    pub fn key(&self) -> &KeyValue {
        &self.key
    }
}

impl PartialOrd for AddressStep {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AddressStep {
    fn cmp(&self, other: &Self) -> Ordering {
        // `NameSegment` carries no `Ord`, so compare its decoded text; a fixed
        // collection name then leaves the key as the sole discriminator, which
        // is exactly the B.5 key-ascending order within one collection.
        self.name
            .as_str()
            .cmp(other.name.as_str())
            .then_with(|| self.key.cmp(&other.key))
    }
}

/// The full address of one row: the ordered steps from the tree root down.
///
/// Non-empty by construction — a row always lives in at least a top-level
/// collection. The derived lexicographic [`Ord`] over the steps places every
/// collection's rows contiguously and in key-ascending order, so a scan is a
/// range walk (Annex B.5).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RowAddress {
    first: AddressStep,
    rest: Vec<AddressStep>,
}

impl RowAddress {
    /// A top-level row address.
    #[must_use]
    pub fn root(step: AddressStep) -> Self {
        Self { first: step, rest: Vec::new() }
    }

    /// Extend this address by one nested collection level.
    #[must_use]
    pub fn child(mut self, step: AddressStep) -> Self {
        self.rest.push(step);
        self
    }

    /// The address steps, root first.
    pub fn steps(&self) -> impl Iterator<Item = &AddressStep> {
        core::iter::once(&self.first).chain(self.rest.iter())
    }

    /// The number of collection levels in this address.
    #[must_use]
    pub fn depth(&self) -> usize {
        1 + self.rest.len()
    }

    /// The collection this row belongs to: its address minus the final key.
    #[must_use]
    pub fn collection(&self) -> CollectionPath {
        match self.rest.split_last() {
            None => CollectionPath { ancestors: Vec::new(), name: self.first.name.clone() },
            Some((last, ancestors)) => {
                let mut steps = Vec::with_capacity(ancestors.len() + 1);
                steps.push(self.first.clone());
                steps.extend_from_slice(ancestors);
                CollectionPath { ancestors: steps, name: last.name.clone() }
            }
        }
    }

    /// Whether this address names a row strictly below `root` reached only through
    /// the nested-collection `steps` — the oracle membership `scan_subtree`
    /// enumerates (§7.6 shape-directed descent). It holds iff `root` is a strict
    /// prefix of this address and every step past the prefix names a collection in
    /// `steps`. Because a well-formed store holds a stored child only under a
    /// declared nested collection, restricting the descent to `steps` (the shape's
    /// declared nested-collection names) still reaches every descendant row.
    #[must_use]
    pub fn descends_from(&self, root: &RowAddress, steps: &[String]) -> bool {
        if self.depth() <= root.depth() {
            return false;
        }
        let mut own = self.steps();
        for root_step in root.steps() {
            match own.next() {
                Some(step) if step == root_step => {}
                _ => return false,
            }
        }
        own.all(|step| steps.iter().any(|name| name == step.name.as_str()))
    }

    /// A rendering for diagnostics: `/name/key/name/key…` over canonical-JSON
    /// key text. Not the D.3 display path (that needs the schema); enough to name
    /// an address in a [`StoreError`].
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for step in self.steps() {
            out.push('/');
            out.push_str(step.name.as_str());
            out.push('/');
            out.push_str(&step.key.render());
        }
        out
    }
}

/// A collection's address: the ancestor row steps plus the collection's own
/// declaration name (no row key). This is what [`crate::InstanceStore::scan`]
/// enumerates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionPath {
    ancestors: Vec<AddressStep>,
    name: NameSegment,
}

impl CollectionPath {
    /// A top-level collection.
    #[must_use]
    pub fn top(name: NameSegment) -> Self {
        Self { ancestors: Vec::new(), name }
    }

    /// A nested collection under the given ancestor row steps.
    #[must_use]
    pub fn nested(ancestors: impl IntoIterator<Item = AddressStep>, name: NameSegment) -> Self {
        Self { ancestors: ancestors.into_iter().collect(), name }
    }

    /// The collection declaration name.
    #[must_use]
    pub fn name(&self) -> &NameSegment {
        &self.name
    }

    /// The address of a specific row in this collection.
    #[must_use]
    pub fn row(&self, key: KeyValue) -> RowAddress {
        let step = AddressStep::new(self.name.clone(), key);
        match self.ancestors.split_first() {
            None => RowAddress::root(step),
            Some((first, rest)) => {
                let mut addr = RowAddress::root(first.clone());
                for ancestor in rest {
                    addr = addr.child(ancestor.clone());
                }
                addr.child(step)
            }
        }
    }

    /// Whether `address` is a direct row of this collection (same ancestors,
    /// same collection name, one final key step and nothing deeper).
    #[must_use]
    pub fn contains(&self, address: &RowAddress) -> bool {
        if address.depth() != self.ancestors.len() + 1 {
            return false;
        }
        let mut steps = address.steps();
        for ancestor in &self.ancestors {
            match steps.next() {
                Some(step) if step == ancestor => {}
                _ => return false,
            }
        }
        match steps.next() {
            Some(step) => step.name.as_str() == self.name.as_str(),
            None => false,
        }
    }
}

/// Build a [`KeyValue`] from a possibly-empty component list, rejecting empty.
///
/// The typed constructors keep an empty key unrepresentable, but a decoder that
/// receives components as a slice needs this fallible bridge (§5.4).
pub fn key_from_components(components: Vec<Value>) -> Result<KeyValue, StoreError> {
    let mut iter = components.into_iter();
    match iter.next() {
        Some(first) => Ok(KeyValue::composite(first, iter)),
        None => Err(StoreError::Corruption {
            detail: "a key must have at least one component".to_owned(),
        }),
    }
}
