//! Addressing a mounted module instance (§13.2/§13.3).
//!
//! A module is a value living in one entry of an ordinary **map collection**
//! (`{ $key: text, $value: module }`), so an instance is addressed by the
//! [`RowAddress`] of that entry — the same address every other row in the tree
//! has. There is no module-space coordinate system: the containing rows are
//! whatever the containing collections already give, at any nesting depth, and
//! `.modules[@id]` is plain map indexing resolved by the interpreter's ordinary
//! collection-reference machinery.
//!
//! What this module owns is the readings of that address the module host needs,
//! each a projection of the address itself rather than a second identity: the
//! **instance name** (the entry key), the **declaration path** (which compiled
//! module collection declares its boundary contracts), and the containing-row
//! steps a §13.2 liveness check walks. The sibling set needs nothing here — two
//! entries are siblings exactly when `RowAddress::collection` agrees, which is
//! ordinary collection membership.

use liasse_store::{AddressStep, CollectionPath, KeyValue, RowAddress};
use liasse_value::{Text, Value};

use crate::modules::ModuleError;

/// The instance name of a module entry (§13.3: "a non-empty text value that forms
/// the local component of instance identity"): the entry's own map key.
///
/// `None` when the key is not a single text value — a module collection declares
/// `$key: text`, so anything else is not an instance slot and the caller refuses
/// rather than rendering a name.
#[must_use]
pub(crate) fn instance_name(at: &RowAddress) -> Option<&str> {
    let step = at.steps().last()?;
    match step.key().components().collect::<Vec<_>>().as_slice() {
        [Value::Text(text)] => Some(text.as_str()),
        _ => None,
    }
}

/// The instance name, or a [`ModuleError::EmptyName`] naming the address. §13.3
/// makes the name a non-empty text value; an entry keyed by anything else
/// addresses no instance.
pub(crate) fn require_instance_name(at: &RowAddress) -> Result<&str, ModuleError> {
    match instance_name(at) {
        Some(name) if !name.is_empty() => Ok(name),
        _ => Err(ModuleError::EmptyName),
    }
}

/// The declaration-name path of the module collection this entry belongs to
/// (`/companies/acme/modules/kit` → `["companies", "modules"]`): the collection
/// names of the address's steps. This keys the entry against the root package's
/// compiled module-collection declarations, and it is derived from the address
/// rather than carried beside it, so the two cannot drift.
#[must_use]
pub(crate) fn declaration_path(at: &RowAddress) -> Vec<String> {
    at.steps().map(|step| step.name().as_str().to_owned()).collect()
}

/// The address of the entry named `name` in the same module collection as `at` —
/// the destination of a §13.3 rename / §13.16 within-collection relocate.
#[must_use]
pub(crate) fn sibling_named(at: &RowAddress, name: &str) -> RowAddress {
    at.collection().row(KeyValue::single(Value::Text(Text::new(name))))
}

/// The address of entry `name` in the module collection `collection`.
#[must_use]
pub(crate) fn entry(collection: &CollectionPath, name: &str) -> RowAddress {
    collection.row(KeyValue::single(Value::Text(Text::new(name))))
}

/// The `(collection declaration name, D.2 key text)` steps of a module entry's
/// **containing row**, as [`crate::Engine::contains_row`] takes them: the address
/// minus its own module step, read as the alternating collection/key pairs that
/// address the row the module collection hangs off. A top-level module collection
/// yields an empty vector — its container is the package root, which is always
/// live.
///
/// `None` when a containing key has no D.2 key text, which names no row.
pub(crate) fn containing_row_steps(at: &RowAddress) -> Option<Vec<(String, String)>> {
    let steps: Vec<&AddressStep> = at.steps().collect();
    let (_own, ancestors) = steps.split_last()?;
    ancestors
        .iter()
        .map(|step| {
            let components: Vec<Value> = step.key().components().cloned().collect();
            liasse_ident::KeyText::from_key_values(&components)
                .ok()
                .map(|key| (step.name().as_str().to_owned(), key.as_str().to_owned()))
        })
        .collect()
}

/// The key [`Value`] of one address step, in the form a materialized row carries
/// it (§5.4): the lone component of a single-field key, or the positional
/// [`Value::Composite`] of a composite one.
#[must_use]
pub(crate) fn step_key_value(step: &AddressStep) -> Value {
    let mut components = step.key().components().cloned();
    match (components.next(), components.collect::<Vec<_>>()) {
        (Some(first), rest) if rest.is_empty() => first,
        (Some(first), rest) => {
            Value::Composite(std::iter::once(first).chain(rest).collect())
        }
        (None, _) => Value::None,
    }
}
