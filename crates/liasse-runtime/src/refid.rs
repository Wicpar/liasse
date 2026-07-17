//! Application-visible key identity for references (§5.4, §6.3, §7.6, A.9).
//!
//! A row's application-visible key (§5.4) and the key a reference denotes are one
//! and the same value: a single-field `$key` is its bare scalar, and a composite
//! `$key` is the name-sorted struct a row materializes (`materialize::key_identity`)
//! and a ref to a composite target decodes to (a ref is typed
//! `RefTarget::Scalar(Struct)`, so it is carried as `Ref::scalar(Struct)`).
//!
//! Normalizing every carrier — a row's key components, a `Ref`, or a bare stored
//! key — through this one identity is what lets reference admission
//! (`rules::target_present`) and inbound-ref rewrite on rekey (`interp`) match a
//! composite reference against its target uniformly, reconciling the naming and
//! `$key`-order-vs-name-sorted difference the positional tuple and the row's
//! struct otherwise disagree on.

use liasse_value::{RefKey, Struct, Text, Value};

/// The application-visible key value (§5.4) from a collection's `$key` field
/// `names` and a row's ordered key `components`: a lone component is its bare
/// scalar, several become a name-sorted struct.
pub(crate) fn identity_of(names: &[String], components: &[Value]) -> Value {
    match names {
        [_] => components.first().cloned().unwrap_or(Value::None),
        _ => Value::Struct(Struct::new(
            names
                .iter()
                .zip(components)
                .map(|(name, value)| (Text::new(name.clone()), value.clone())),
        )),
    }
}

/// The application key a reference denotes (§6.3/A.9): a scalar-keyed ref exposes
/// its bare value (a scalar for a single-field target, or the name-sorted struct
/// a composite ref decodes to); a positional composite ref is reconciled to that
/// same struct by pairing its `$key`-order components with the target's key field
/// `names`.
pub(crate) fn ref_identity(names: &[String], key: &RefKey) -> Value {
    match key {
        RefKey::Scalar(value) => (**value).clone(),
        RefKey::Composite(components) => identity_of(names, components),
    }
}
