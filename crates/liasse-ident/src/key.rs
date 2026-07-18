//! Canonical key text (Annex D.2).
//!
//! A [`KeyComponent`] is the canonical scalar text of one key field, held in
//! its raw (unescaped) form. A [`KeyText`] is a full key: one or more
//! components, each escaped, joined by `:` in `$key` order — the exact text
//! that names a seed object member, a display-path key segment, and a canonical
//! textual export. Expressions never use this text; they use typed key values
//! (D.2), so this module renders and parses text but does not re-type it.

use liasse_value::{RefKey, Value};

use crate::error::IdentError;
use crate::escape::Codec;

/// The canonical scalar key text of one key field (D.2), stored decoded.
///
/// Construction from a [`Value`] pins each scalar's canonical spelling exactly
/// as D.2's table does: `bool` is `true`/`false`, `uuid` is lowercase
/// hyphenated, `int`/`decimal`/`date`/`timestamp`/`duration` reuse their
/// liasse-value canonical text, `text` is preserved exactly (no Unicode
/// normalization — canonically-equivalent code point sequences stay distinct).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyComponent(String);

impl KeyComponent {
    /// Wrap raw component text directly (e.g. a decoded seed member name).
    #[must_use]
    pub fn from_text(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// Render the D.2 canonical scalar text of a single scalar key value.
    ///
    /// Errors for a [`Value`] that D.2 gives no scalar key text. A `struct` or
    /// `ref` key is not a single scalar component: it flattens to several
    /// components, so build those through [`KeyText::from_key_values`].
    pub fn from_scalar(value: &Value) -> Result<Self, IdentError> {
        let text = match value {
            Value::Text(t) => t.as_str().to_owned(),
            Value::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
            Value::Int(i) => i.to_canonical_text(),
            Value::Decimal(d) => d.to_canonical_text(),
            Value::Bytes(b) => b.to_base64(),
            Value::Uuid(u) => u.to_canonical_text(),
            Value::Date(d) => d.to_canonical_text(),
            Value::Timestamp(t) => t.to_canonical_text(),
            Value::Duration(d) => d.to_canonical_text(),
            Value::Enum(e) => e.label().to_owned(),
            other => {
                return Err(IdentError::NotKeyComponent {
                    type_name: Self::variant_name(other),
                });
            }
        };
        Ok(Self(text))
    }

    /// The raw (decoded) component text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The escaped component text (reserved `%`, `/`, `:` percent-encoded).
    #[must_use]
    pub fn encode(&self) -> String {
        Codec::KEY.encode(&self.0)
    }

    /// Decode one escaped component back to its raw text.
    pub fn decode(encoded: &str) -> Result<Self, IdentError> {
        Codec::KEY.decode(encoded).map(Self)
    }

    fn variant_name(value: &Value) -> &'static str {
        match value {
            Value::Text(_) => "text",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Decimal(_) => "decimal",
            Value::Bytes(_) => "bytes",
            Value::Uuid(_) => "uuid",
            Value::Date(_) => "date",
            Value::Timestamp(_) => "timestamp",
            Value::Duration(_) => "duration",
            Value::Period(_) => "period",
            Value::Json(_) => "json",
            Value::Blob(_) => "blob",
            Value::Enum(_) => "enum",
            Value::Ref(_) => "ref",
            Value::Struct(_) => "struct",
            Value::Composite(_) => "composite key",
            Value::Set(_) => "set",
            Value::Map(_) => "map",
            Value::None => "none",
        }
    }
}

/// The canonical, escaped, `:`-joined text of a full key (D.2).
///
/// Held in escaped form: this is precisely the seed object member name, the
/// display-path key segment, and the canonical export text for the key. A
/// single-field key is one component; a composite key is several joined by `:`
/// in `$key` order. Escaping happens per component *before* the join (D.2), so
/// a `/` or `:` inside a component cannot be confused with the join separator
/// or a path separator.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyText(String);

impl KeyText {
    /// Join one or more components into canonical key text. The `first`/`rest`
    /// split makes an empty key unrepresentable (§5.4: a key names at least one
    /// field).
    #[must_use]
    pub fn new(first: &KeyComponent, rest: &[KeyComponent]) -> Self {
        let mut joined = first.encode();
        for component in rest {
            joined.push(':');
            joined.push_str(&component.encode());
        }
        Self(joined)
    }

    /// Build canonical key text from the typed key values in `$key` order.
    ///
    /// Each value is flattened to its scalar components: a scalar contributes
    /// one; a `struct` contributes its fields in canonical field-name order
    /// (D.2 "its components in canonical field-name order"); a `ref`
    /// contributes its target key's components (D.1). Errors on an empty key or
    /// a non-key-eligible value.
    pub fn from_key_values(values: &[Value]) -> Result<Self, IdentError> {
        let mut components = Vec::new();
        for value in values {
            Self::flatten(value, &mut components)?;
        }
        let (first, rest) = components
            .split_first()
            .ok_or(IdentError::EmptyKey)?;
        Ok(Self::new(first, rest))
    }

    /// The canonical escaped key text — the seed member name / path key
    /// segment.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse an already-escaped key text (a seed member name), validating that
    /// every escape is canonical. The text is stored verbatim; call
    /// [`KeyText::components`] to recover the decoded components.
    pub fn parse(encoded: impl Into<String>) -> Result<Self, IdentError> {
        let text = encoded.into();
        Codec::KEY.validate(&text)?;
        Ok(Self(text))
    }

    /// The decoded components, split on the unescaped `:` join separators (an
    /// original `:` inside a component is `%3A`, so this split is unambiguous).
    pub fn components(&self) -> Result<Vec<KeyComponent>, IdentError> {
        self.0.split(':').map(KeyComponent::decode).collect()
    }

    fn flatten(value: &Value, out: &mut Vec<KeyComponent>) -> Result<(), IdentError> {
        match value {
            Value::Struct(s) => {
                for (_, field) in s.fields() {
                    Self::flatten(field, out)?;
                }
                Ok(())
            }
            Value::Ref(r) => match r.key() {
                RefKey::Scalar(inner) => Self::flatten(inner, out),
                RefKey::Composite(components) => {
                    for component in components {
                        Self::flatten(component, out)?;
                    }
                    Ok(())
                }
            },
            // D.2: a composite key flattens to its components in `$key` order (the
            // sequence order), joined by `:` — distinct from a struct's
            // field-name order.
            Value::Composite(components) => {
                for component in components {
                    Self::flatten(component, out)?;
                }
                Ok(())
            }
            scalar => {
                out.push(KeyComponent::from_scalar(scalar)?);
                Ok(())
            }
        }
    }
}
