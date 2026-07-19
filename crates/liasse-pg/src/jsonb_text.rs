//! NUL-safe text encoding for the store's `jsonb` **and** raw `text` columns.
//!
//! Annex A.1 defines `text` as a sequence of Unicode scalar values and does not
//! exclude `U+0000` (NUL); `Text::new` is total, so `Value::Text("a\0b")` is a
//! well-formed Liasse value the in-memory reference persists verbatim. The
//! contract binds both backends to *identical* observable results, but PostgreSQL
//! rejects a raw `U+0000` in **either** storage form — inside `jsonb` (`ERROR:
//! unsupported Unicode escape sequence`, SQLSTATE 22P05) and inside a `text`/
//! `varchar` value (`ERROR: invalid byte sequence for encoding "UTF8": 0x00`,
//! SQLSTATE 22021). So neither the value/op/composition trees this crate stores in
//! `jsonb` columns nor the opaque-token identities it stores in `text` columns
//! (`commit_log.transaction_id`, `history_points.lineage`/`point`,
//! `instance_meta.definition_source`/`instance_id` — all unvalidated D.5 tokens or
//! D.4 source text) may carry one. SPEC-ISSUES item 32 tracks the spec gap;
//! regardless of how it resolves, the two backends must agree, which is what this
//! module guarantees for both column kinds.
//!
//! [`to_jsonb`] (for `jsonb` leaves) and [`encode_text`] (for a bare `text` token)
//! share one reversible escape that is a total inverse of [`from_jsonb`] /
//! [`decode_text`]:
//!
//! - `U+0000` becomes `\0` (reverse solidus then `0`),
//! - `\` (reverse solidus) becomes `\\`,
//! - every other scalar is left exactly as-is.
//!
//! Escaping the escape character keeps the mapping unambiguous, so decode is
//! deterministic and lossless for any Unicode text. Only strings that actually
//! contain `U+0000` or `\` change shape; all other stored text — canonical ints,
//! base64, uuids, `row-N` tokens — is untouched. The escape is a bijection, so it
//! also preserves the equality lookups a `text` column is queried by (the
//! `history_points (lineage, point)` primary key: equal tokens escape to equal
//! stored text, distinct to distinct).
//!
//! **Scan order is unaffected.** The store never orders rows by the stored `jsonb`
//! text: a scan is ordered by the `key_enc` BYTEA column, whose memcmp order over a
//! fixed `(parent_id, step_name)` *is* Annex B order (§4.2), and each result key is
//! rebuilt by decoding `key_wire` back to the typed [`liasse_value::Value`]. This
//! encoding round-trips those key values exactly, so the decoded addresses — and
//! therefore the scan order — are identical to the in-memory reference's.
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

/// NUL-safe encode a bare opaque token for storage in a raw `text` column — the
/// same reversible escape [`to_jsonb`] applies to `jsonb` string leaves. An
/// identity token (`TransactionId`, `LineageId`, `PointId`) or definition source
/// is arbitrary A.1 text and may carry a `U+0000` a PostgreSQL `text` value cannot
/// hold. Symmetric with [`decode_text`]; write every such column through this and
/// read every one back through [`decode_text`].
#[must_use]
pub fn encode_text(token: &str) -> String {
    escape(token)
}

/// Invert [`encode_text`]: recover an opaque token (including any `U+0000`) from a
/// `text` column written by it.
#[must_use]
pub fn decode_text(stored: &str) -> String {
    unescape(stored)
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
