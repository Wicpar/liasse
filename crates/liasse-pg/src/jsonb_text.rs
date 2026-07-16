//! NUL-safe text encoding for the store's `jsonb` columns.
//!
//! Annex A.1 defines `text` as a sequence of Unicode scalar values and does not
//! exclude `U+0000` (NUL); `Text::new` is total, so `Value::Text("a\0b")` is a
//! well-formed Liasse value the in-memory reference persists verbatim. The
//! contract binds both backends to *identical* observable results, but PostgreSQL
//! `jsonb` cannot hold a raw `U+0000` (`ERROR: unsupported Unicode escape
//! sequence`, SQLSTATE 22P05), so the value/op/composition trees this crate stores
//! in `jsonb` columns must not carry one. SPEC-ISSUES item 32 tracks the spec gap;
//! regardless of how it resolves, the two backends must agree, which is what this
//! module guarantees.
//!
//! [`to_jsonb`] rewrites every JSON string leaf so no `U+0000` survives, using a
//! C-style escape that is a total inverse of [`from_jsonb`]:
//!
//! - `U+0000` becomes `\0` (reverse solidus then `0`),
//! - `\` (reverse solidus) becomes `\\`,
//! - every other scalar is left exactly as-is.
//!
//! Escaping the escape character keeps the mapping unambiguous, so decode is
//! deterministic and lossless for any Unicode text. Only strings that actually
//! contain `U+0000` or `\` change shape; all other stored text — canonical ints,
//! base64, uuids — is untouched.
//!
//! **Scan order is unaffected.** The store never orders rows by the stored `jsonb`
//! text: [`crate::projection::Projection`] holds a `BTreeMap` keyed by the decoded
//! [`RowAddress`], whose [`Ord`] is Annex B (Unicode scalar order) over the typed
//! [`liasse_value::Value`] key. This encoding round-trips those key values exactly,
//! so the decoded addresses — and therefore the scan order — are identical to the
//! in-memory reference's.
//!
//! [`RowAddress`]: liasse_store::RowAddress

use serde_json::{Map, Value as J};

/// Rewrite `value` so every string leaf is safe to store in a PostgreSQL `jsonb`
/// column: no leaf carries a raw `U+0000`. The structural shape is preserved and
/// the transform is inverted exactly by [`from_jsonb`].
#[must_use]
pub fn to_jsonb(value: &J) -> J {
    map_strings(value, escape)
}

/// Invert [`to_jsonb`]: recover the original string leaves (including any
/// `U+0000`) from a tree read back out of a `jsonb` column.
#[must_use]
pub fn from_jsonb(value: &J) -> J {
    map_strings(value, unescape)
}

/// Rebuild `value`, applying `f` to every string leaf and recursing through
/// arrays and objects. Object keys are mapped too: a struct field name or map key
/// text can itself carry a `U+0000`.
fn map_strings(value: &J, f: fn(&str) -> String) -> J {
    match value {
        J::String(text) => J::String(f(text)),
        J::Array(items) => J::Array(items.iter().map(|item| map_strings(item, f)).collect()),
        J::Object(members) => J::Object(
            members.iter().map(|(key, member)| (f(key), map_strings(member, f))).collect::<Map<_, _>>(),
        ),
        other => other.clone(),
    }
}

/// Escape `U+0000` (as `\0`) and the escape character `\` (as `\\`); pass every
/// other scalar through unchanged.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for scalar in text.chars() {
        match scalar {
            '\\' => out.push_str("\\\\"),
            '\u{0}' => out.push_str("\\0"),
            other => out.push(other),
        }
    }
    out
}

/// Invert [`escape`]. A `\` introduces exactly one of the two escapes `escape`
/// produces (`\\` or `\0`); a lone trailing or unrecognized `\` (only reachable
/// from corrupt durable bytes) is passed through verbatim so decoding stays total
/// and never panics.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut scalars = text.chars();
    while let Some(scalar) = scalars.next() {
        if scalar != '\\' {
            out.push(scalar);
            continue;
        }
        match scalars.next() {
            Some('\\') => out.push('\\'),
            Some('0') => out.push('\u{0}'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}
