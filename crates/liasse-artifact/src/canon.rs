//! Canonical strict-JSON encoding for `manifest.json` (SPEC.md §19.5, D.5).
//!
//! D.5 requires `manifest.json` to be strict canonical JSON, and D.4 pins the
//! canonical ordering: "object member names sorted by Unicode scalar order".
//! The manifest is a closed structure of strings, one integer, and nested
//! string-keyed objects, so this small model captures exactly what it needs.
//!
//! Encoding does **not** go through `serde_json::Value`: workspace feature
//! unification turns on serde_json's `preserve_order`, under which a `Value`
//! object serializes in insertion order rather than sorted order. A
//! [`Json::Object`] is a [`BTreeMap`], so member order is Unicode-scalar order
//! by construction, independent of any feature. String *escaping* is delegated
//! to `serde_json` so the byte-level escaping matches the rest of the workspace.

use std::collections::BTreeMap;

/// A canonical JSON node: exactly the shapes `manifest.json` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Json {
    /// A JSON string.
    Str(String),
    /// A non-negative JSON integer (the manifest's only number is `format: 1`).
    Int(u64),
    /// A JSON boolean (the manifest's `coverage.fully_restorable`).
    Bool(bool),
    /// A JSON object, always encoded in Unicode-scalar key order.
    Object(BTreeMap<String, Json>),
}

impl Json {
    /// A convenience constructor for a string node.
    #[must_use]
    pub fn str(text: impl Into<String>) -> Self {
        Self::Str(text.into())
    }

    /// Encode to canonical strict-JSON bytes.
    #[must_use]
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        self.write(&mut out);
        out.into_bytes()
    }

    /// Build an object from `(name, node)` pairs. Callers never pass duplicate
    /// names; a duplicate would collapse in the [`BTreeMap`], which is the
    /// canonical outcome anyway.
    #[must_use]
    pub fn object(members: impl IntoIterator<Item = (String, Json)>) -> Self {
        Self::Object(members.into_iter().collect())
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Str(text) => Self::write_string(text, out),
            Self::Int(n) => out.push_str(&n.to_string()),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Object(members) => {
                out.push('{');
                for (i, (key, value)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    Self::write_string(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }

    /// Append `text` as a JSON string literal with canonical escaping. String
    /// escaping is delegated to `serde_json` (which never fails on a plain
    /// string) so it matches the rest of the workspace byte-for-byte; the
    /// impossible error falls back to a minimal manual escape rather than panic.
    fn write_string(text: &str, out: &mut String) {
        match serde_json::to_string(text) {
            Ok(encoded) => out.push_str(&encoded),
            Err(_) => {
                out.push('"');
                for ch in text.chars() {
                    match ch {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
        }
    }
}
