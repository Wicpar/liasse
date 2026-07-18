//! Application-visible key identity for references (§5.4, §6.3, §7.6, A.9).
//!
//! A row's application-visible key (§5.4) and the key a reference denotes are one
//! and the same value: a single-field `$key` is its bare scalar, and a composite
//! `$key` is the positional [`Value::Composite`] tuple in `$key` order — the same
//! value a row materializes (`materialize::key_identity`) and a composite ref
//! carries as its positional [`RefKey::Composite`].
//!
//! Because the composite carrier is positional (`$key` order, no field names),
//! this identity is name-free: pairing a `Ref`'s components with the same value a
//! row's components produce lets reference admission (`rules::target_present`),
//! inbound-ref rewrite on rekey (`interp`), and the `$on_delete` cascade
//! (`cascade`) match a composite reference against its target by value equality.

use liasse_value::{Ref, RefKey, Value};

/// The application-visible key value (§5.4) from a collection's `$key` field
/// `names` and a row's ordered key `components`: a lone component is its bare
/// scalar, several become the positional [`Value::Composite`] tuple in `$key`
/// order (the `names` fix only the single-vs-composite arity — the composite
/// value carries no field names, it is positional per B.4).
pub(crate) fn identity_of(names: &[String], components: &[Value]) -> Value {
    match names {
        [_] => components.first().cloned().unwrap_or(Value::None),
        _ => Value::Composite(components.to_vec()),
    }
}

/// The application key a reference denotes (§6.3/A.9): a scalar-keyed ref exposes
/// its bare value; a composite ref exposes the positional [`Value::Composite`]
/// tuple of its `$key`-order components — the identical value the target row
/// materializes, so the two compare equal by value.
pub(crate) fn ref_identity(names: &[String], key: &RefKey) -> Value {
    match key {
        RefKey::Scalar(value) => (**value).clone(),
        RefKey::Composite(components) => identity_of(names, components),
    }
}

/// The `Ref` carrier a reference to a collection with key field `names` uses for
/// a target row whose ordered key `components` are given (§5.4 rekey reissue, A.9):
/// a single-field target is a scalar-keyed ref, a composite target a positional
/// composite-keyed ref — the same carrier the model's decode produces, so a
/// rewritten inbound reference keeps the collection's uniform ref shape (and its
/// B.4 ordering).
pub(crate) fn ref_of(names: &[String], components: &[Value]) -> Ref {
    match names {
        [_] => Ref::scalar(components.first().cloned().unwrap_or(Value::None)),
        _ => Ref::composite(components.to_vec()),
    }
}
