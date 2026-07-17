//! The §13.13 three-way seed merge.

use std::collections::BTreeMap;

use liasse_value::Value;

/// The §13.13 three-way seed merge over one row's fields: for each seeded field,
/// the new package seed replaces the value only when the current value still
/// equals the old package seed value; otherwise the current value is retained as
/// local data. On update, this reconciles a changed package seed against local
/// edits without clobbering them (§13.13 "the new seed replaces the value only
/// when the current value still equals the old seed value").
///
/// This is the pure rule; keyed-child-collection recursion and set membership
/// reconciliation (also in §13.13) apply the same field rule per key and per
/// member, and wiring the rule into the child engine's update seed pass is a
/// documented seam (the engine's migration owns seed application).
pub struct SeedMerge<'a> {
    /// The old package seed values.
    pub old_seed: &'a BTreeMap<String, Value>,
    /// The new package seed values.
    pub new_seed: &'a BTreeMap<String, Value>,
    /// The current instance state.
    pub current: &'a BTreeMap<String, Value>,
}

impl SeedMerge<'_> {
    /// Compute the merged field map (§13.13).
    #[must_use]
    pub fn merge(&self) -> BTreeMap<String, Value> {
        let mut merged = BTreeMap::new();
        let mut names: Vec<&String> = self
            .old_seed
            .keys()
            .chain(self.new_seed.keys())
            .chain(self.current.keys())
            .collect();
        names.sort();
        names.dedup();
        for name in names {
            if let Some(value) = Self::merge_field(
                self.old_seed.get(name),
                self.new_seed.get(name),
                self.current.get(name),
            ) {
                merged.insert(name.clone(), value);
            }
        }
        merged
    }

    /// Merge one field: replace with the new seed only when the current value is
    /// still the old seed value; otherwise keep the current (local) value.
    fn merge_field(old: Option<&Value>, new: Option<&Value>, current: Option<&Value>) -> Option<Value> {
        if current == old {
            new.cloned()
        } else {
            current.cloned()
        }
    }
}
