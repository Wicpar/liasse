//! Order-preserving `BYTEA` encoding of an evaluated `$sort` tuple (§7.4 of
//! `DESIGN-pure-pg.md`) — the `ORDER BY` key the pushdown `liasse.eval_sort` face
//! emits.
//!
//! One encoded tuple is the plain concatenation of its per-key units. Each key
//! unit is the value's order-preserving [`key_enc`] bytes (which already place
//! `none` last and are prefix-free and canonical across every [`Value`] class),
//! **with every byte inverted for a descending key**. Inverting a whole
//! prefix-free-and-canonical unit reverses that key's memcmp order — placing `none`
//! first and the present values descending (§7.3) — while preserving
//! prefix-freeness and canonicality, so the concatenation's `memcmp` reproduces the
//! mixed-direction tuple comparison exactly:
//!
//! ```text
//! sign(memcmp(encode(a, dirs), encode(b, dirs))) == sign(tuple_cmp(a, b, dirs))
//! ```
//!
//! where `tuple_cmp` compares successive keys under [`Value`]'s Annex-B `Ord`,
//! reversing each descending key, and ties on all keys. The occurrence tiebreak is
//! deliberately NOT encoded here: the SQL appends the key-path columns as trailing
//! `ORDER BY` terms (the D.1 occurrence identity). That correspondence — against
//! the shared `SortOrder::compare` the in-Rust oracle sorts by — is the property
//! `liasse-pred`'s `sort_enc` proptest pins.

use liasse_store::SortDirection;
use liasse_value::Value;

use crate::key_enc;

/// Encode one evaluated `$sort` tuple to its order-preserving bytes, per key under
/// its direction. A key with no matching direction defaults to ascending.
#[must_use]
pub fn encode_sort_tuple(tuple: &[Value], directions: &[SortDirection]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, value) in tuple.iter().enumerate() {
        let descending = matches!(directions.get(index), Some(SortDirection::Descending));
        encode_sort_key(value, descending, &mut out);
    }
    out
}

/// Append one key's order-preserving unit: the value's [`key_enc`] bytes, inverted
/// wholesale for a descending key.
fn encode_sort_key(value: &Value, descending: bool, out: &mut Vec<u8>) {
    let mut unit = Vec::new();
    key_enc::encode_value(value, &mut unit);
    if descending {
        for byte in &mut unit {
            *byte = !*byte;
        }
    }
    out.extend_from_slice(&unit);
}
