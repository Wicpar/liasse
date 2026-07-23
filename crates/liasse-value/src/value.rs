//! The runtime [`Value`] and its canonical wire encoding and total order.

use core::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use crate::blob::BlobDescriptor;
use crate::decimal::Decimal;
use crate::duration::Duration;
use crate::enumeration::EnumValue;
use crate::int::Integer;
use crate::json::Json;
use crate::period::Period;
use crate::scalars::{Bytes, Text, Uuid};
use crate::temporal::{Date, Timestamp};

/// Compare two optional structural members with **`none` last** (SPEC-ISSUES
/// item 30 / B.4): present members compare by their inner value, and an absent
/// (`None`) member sorts *after* any present one, consistent with B.2's
/// present-before-`none`. This is the opposite of `Option`'s derived order
/// (`None` first), so descriptor members (a blob `$name`, a calendar period
/// `zone`) that omit an optional field sort last within B.4 structural order.
pub(crate) fn cmp_optional_none_last<T: Ord>(a: &Option<T>, b: &Option<T>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// A ref's typed key value (A.9): a single scalar key, or a tuple of component
/// values in `$key` order. Never the D.2 colon-joined text.
#[derive(Debug, Clone)]
pub enum RefKey {
    Scalar(Box<Value>),
    Composite(Vec<Value>),
}

/// A checked reference to a target row, carried as its typed key (A.9).
/// Ordering (B.1) is target-key order.
#[derive(Debug, Clone)]
pub struct Ref(RefKey);

impl Ref {
    /// A scalar-keyed ref.
    #[must_use]
    pub fn scalar(key: Value) -> Self {
        Self(RefKey::Scalar(Box::new(key)))
    }

    /// A composite-keyed ref, components in `$key` order.
    #[must_use]
    pub fn composite(components: Vec<Value>) -> Self {
        Self(RefKey::Composite(components))
    }

    /// The typed key.
    #[must_use]
    pub fn key(&self) -> &RefKey {
        &self.0
    }

    fn to_wire(&self) -> serde_json::Value {
        match &self.0 {
            RefKey::Scalar(value) => value.to_wire(),
            RefKey::Composite(components) => {
                serde_json::Value::Array(components.iter().map(Value::to_wire).collect())
            }
        }
    }
}

/// A static struct value: named fields keyed by name. The [`BTreeMap`] holds
/// fields in field-name text order — the canonical comparison order (B.4) and
/// the canonical wire key order (A.7).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Struct(BTreeMap<Text, Value>);

impl Struct {
    /// Assemble from name/value pairs.
    #[must_use]
    pub fn new(fields: impl IntoIterator<Item = (Text, Value)>) -> Self {
        Self(fields.into_iter().collect())
    }

    /// The fields in canonical (text) order.
    pub fn fields(&self) -> impl Iterator<Item = (&Text, &Value)> {
        self.0.iter()
    }

    /// Look up a field by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(&Text::new(name))
    }
}

/// A canonical Liasse runtime value (Annex A). Every variant is well-formed by
/// construction: there is no way to build, say, a non-decimal `Decimal`.
#[derive(Debug, Clone)]
pub enum Value {
    Text(Text),
    Bool(bool),
    Int(Integer),
    Decimal(Decimal),
    Bytes(Bytes),
    Uuid(Uuid),
    Date(Date),
    Timestamp(Timestamp),
    Duration(Duration),
    Period(Box<Period>),
    Json(Json),
    Blob(Box<BlobDescriptor>),
    Enum(EnumValue),
    Ref(Ref),
    Struct(Struct),
    /// A composite key value (A.9, B.4): its component values in `$key` order.
    /// Ordering is positional over that sequence — the B.4 "composite key" rule,
    /// which is distinct from a struct's field-name order — and the wire form is
    /// the `$key`-order array of component wire values. This is the sole carrier
    /// of a composite row's application-visible key identity (§5.4); a composite
    /// `ref`'s target key is the equal-valued positional [`RefKey::Composite`].
    Composite(Vec<Value>),
    Set(BTreeSet<Value>),
    Map(BTreeMap<Value, Value>),
    /// The Liasse `none` — the *absence* of an `optional<T>` value (A.1). It is
    /// not a value that can be a member of a set, a map value, or a positional
    /// key component; it is represented by not being there (an omitted struct
    /// member, a non-member of a set, an absent map key) and has no wire sentinel.
    /// The one position that cannot be omitted — a fixed-arity positional
    /// composite optional slot — carries it as JSON `null`. Distinct from JSON
    /// `null` the value (which is `Value::Json(Json::Null)`); the two only share a
    /// wire byte-form in that single positional slot, disambiguated by type.
    /// Ordered last within its type (B.2/B.4) via [`Value::rank`].
    None,
}

impl Value {
    /// The identity form of a key value (Annex D.1/D.2): a `ref` flattens to its
    /// target key value (a scalar ref to that scalar, a composite ref to the
    /// positional component tuple), and a composite key recurses component-wise.
    ///
    /// A key value's identity is therefore independent of whether a `ref`
    /// component is carried as a `Value::Ref` or as its already-dereferenced bare
    /// scalar key (§6.3 ref/key equality) — the value-level analogue of the D.2
    /// key-text flattening (`liasse_ident::KeyText`), so a row identity built from
    /// the stored key and one built from an equal application key agree. Ordering
    /// the identity form matches the underlying key's value order (a `ref` orders
    /// by target key, B.1); every non-ref value is its own identity.
    #[must_use]
    pub fn identity_value(&self) -> Value {
        match self {
            Value::Ref(reference) => match reference.key() {
                RefKey::Scalar(inner) => inner.identity_value(),
                RefKey::Composite(components) => {
                    Value::Composite(components.iter().map(Value::identity_value).collect())
                }
            },
            Value::Composite(components) => {
                Value::Composite(components.iter().map(Value::identity_value).collect())
            }
            other => other.clone(),
        }
    }

    /// Cross-variant rank.
    ///
    /// Annex B defines a total order *within* each type and the null/none
    /// placements, but pins no cross-*type* order (a sort column is
    /// single-typed). `Ord` nonetheless requires a total order across all
    /// values, so this rank supplies a deterministic one. The only
    /// spec-constrained edge is that `None` is the maximum, realizing B.2's
    /// "present values ascending, then none".
    const fn rank(&self) -> u8 {
        match self {
            Self::Bool(_) => 0,
            Self::Int(_) => 1,
            Self::Decimal(_) => 2,
            Self::Text(_) => 3,
            Self::Bytes(_) => 4,
            Self::Uuid(_) => 5,
            Self::Date(_) => 6,
            Self::Timestamp(_) => 7,
            Self::Duration(_) => 8,
            Self::Period(_) => 9,
            Self::Enum(_) => 10,
            Self::Ref(_) => 11,
            Self::Json(_) => 12,
            Self::Blob(_) => 13,
            Self::Struct(_) => 14,
            Self::Set(_) => 15,
            Self::Map(_) => 16,
            Self::Composite(_) => 17,
            Self::None => u8::MAX,
        }
    }

    /// Encode to a canonical strict-JSON value (Annex A).
    #[must_use]
    pub fn to_wire(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Self::Text(t) => J::String(t.as_str().to_owned()),
            Self::Bool(b) => J::Bool(*b),
            Self::Int(i) => J::String(i.to_canonical_text()),
            Self::Decimal(d) => J::String(d.to_canonical_text()),
            Self::Bytes(b) => Self::wrap("$bytes", J::String(b.to_base64())),
            Self::Uuid(u) => J::String(u.to_canonical_text()),
            Self::Date(d) => J::String(d.to_canonical_text()),
            Self::Timestamp(t) => J::String(t.to_canonical_text()),
            Self::Duration(d) => J::String(d.to_canonical_text()),
            Self::Period(p) => Self::period_to_wire(p),
            Self::Json(j) => j.to_wire(),
            Self::Blob(b) => Self::blob_to_wire(b),
            Self::Enum(e) => J::String(e.label().to_owned()),
            Self::Ref(r) => r.to_wire(),
            Self::Struct(s) => Self::struct_to_wire(s),
            // A.9/D.2: a composite key's canonical structured wire value is the
            // array of its component wire values in `$key` order.
            Self::Composite(components) => {
                J::Array(components.iter().map(Value::to_wire).collect())
            }
            Self::Set(members) => J::Array(members.iter().map(Value::to_wire).collect()),
            Self::Map(entries) => J::Array(
                entries
                    .iter()
                    .map(|(k, v)| J::Array(vec![k.to_wire(), v.to_wire()]))
                    .collect(),
            ),
            // A.1 / SPEC-ISSUES item 29: `none` is absence, with no wire sentinel.
            // Where absence is expressed by position (an omitted struct member, a
            // non-member of a set, an absent map key) `none` never reaches this arm.
            // The sole position that cannot be omitted is a fixed-arity positional
            // composite optional slot, whose `none` is JSON `null`; `null` is
            // unambiguous there because it is not a canonical wire form for any
            // scalar type. The `{ "$none": true }` sentinel is removed entirely.
            Self::None => J::Null,
        }
    }

    /// The canonical compact JSON text (A.7): sorted object keys, no
    /// whitespace. Serialization of a well-formed value cannot fail; the
    /// fallback exists only to uphold the no-panic rule.
    #[must_use]
    pub fn to_canonical_json_string(&self) -> String {
        serde_json::to_string(&self.to_wire()).unwrap_or_default()
    }

    fn wrap(key: &str, value: serde_json::Value) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert(key.to_owned(), value);
        serde_json::Value::Object(map)
    }

    /// Assemble a canonical wire object with members in canonical key order.
    ///
    /// Canonical JSON sorts object members by the Liasse text order — Unicode
    /// scalar order over the key (SPEC.md A.7, "sorts object keys by the Liasse
    /// text order", line 4459; Annex D.4, "object member names sorted by Unicode
    /// scalar order", line 5024). Wire member order carries no package semantics
    /// (SPEC.md line 4645), so the §18.1 blob example's `$sha512`-first layout is
    /// illustrative, not the canonical order — the canonical order is sorted.
    ///
    /// We sort here rather than lean on the backing `serde_json::Map`: its key
    /// ordering is a `BTreeMap` (sorted) only while `serde_json`'s
    /// `preserve_order` feature is off, and workspace feature unification (a
    /// sibling crate enables it) flips it to insertion order. Sorting explicitly
    /// keeps the canonical form correct under either feature set.
    fn canonical_object(
        members: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> serde_json::Value {
        let mut sorted: Vec<(String, serde_json::Value)> = members.into_iter().collect();
        sorted.sort_by(|(a, _), (b, _)| a.cmp(b));
        serde_json::Value::Object(sorted.into_iter().collect())
    }

    fn struct_to_wire(value: &Struct) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (name, field) in value.fields() {
            // A.1: an omitted optional field carries `none` by absence.
            if matches!(field, Value::None) {
                continue;
            }
            map.insert(name.as_str().to_owned(), field.to_wire());
        }
        serde_json::Value::Object(map)
    }

    fn blob_to_wire(descriptor: &BlobDescriptor) -> serde_json::Value {
        use serde_json::Value as J;
        let mut members = vec![
            ("$sha512".to_owned(), J::String(descriptor.sha512().to_canonical_text())),
            ("$bytes".to_owned(), J::String(descriptor.byte_count().to_string())),
            ("$media".to_owned(), J::String(descriptor.media().as_str().to_owned())),
        ];
        if let Some(name) = descriptor.name() {
            members.push(("$name".to_owned(), J::String(name.to_owned())));
        }
        Self::canonical_object(members)
    }

    fn period_to_wire(period: &Period) -> serde_json::Value {
        use serde_json::Value as J;
        match period {
            Period::Fixed(duration) => J::String(duration.to_canonical_text()),
            Period::Calendar(calendar) => {
                let (years, months, weeks, days) = calendar.calendar_magnitudes();
                let (overflow, ambiguous, missing) = calendar.policy_keywords();
                let mut members = vec![
                    ("years".to_owned(), J::from(years)),
                    ("months".to_owned(), J::from(months)),
                    ("weeks".to_owned(), J::from(weeks)),
                    ("days".to_owned(), J::from(days)),
                    ("time".to_owned(), J::String(calendar.time().to_canonical_text())),
                    ("overflow".to_owned(), J::String(overflow.to_owned())),
                    ("ambiguous".to_owned(), J::String(ambiguous.to_owned())),
                    ("missing".to_owned(), J::String(missing.to_owned())),
                ];
                if let Some(zone) = calendar.zone() {
                    members.push(("zone".to_owned(), J::String(zone.to_owned())));
                }
                Self::canonical_object(members)
            }
        }
    }
}

impl PartialEq for Ref {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Ref {}
impl PartialOrd for Ref {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Ref {
    fn cmp(&self, other: &Self) -> Ordering {
        match (&self.0, &other.0) {
            (RefKey::Scalar(a), RefKey::Scalar(b)) => a.cmp(b),
            (RefKey::Composite(a), RefKey::Composite(b)) => a.cmp(b),
            (RefKey::Scalar(_), RefKey::Composite(_)) => Ordering::Less,
            (RefKey::Composite(_), RefKey::Scalar(_)) => Ordering::Greater,
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Value {}
impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
            (Self::Bool(a), Self::Bool(b)) => a.cmp(b),
            (Self::Int(a), Self::Int(b)) => a.cmp(b),
            (Self::Decimal(a), Self::Decimal(b)) => a.cmp(b),
            (Self::Bytes(a), Self::Bytes(b)) => a.cmp(b),
            (Self::Uuid(a), Self::Uuid(b)) => a.cmp(b),
            (Self::Date(a), Self::Date(b)) => a.cmp(b),
            (Self::Timestamp(a), Self::Timestamp(b)) => a.cmp(b),
            (Self::Duration(a), Self::Duration(b)) => a.cmp(b),
            (Self::Period(a), Self::Period(b)) => a.cmp(b),
            (Self::Json(a), Self::Json(b)) => a.cmp(b),
            (Self::Blob(a), Self::Blob(b)) => a.cmp(b),
            (Self::Enum(a), Self::Enum(b)) => a.cmp(b),
            (Self::Ref(a), Self::Ref(b)) => a.cmp(b),
            (Self::Struct(a), Self::Struct(b)) => a.cmp(b),
            // B.4 composite key: lexicographic components in `$key` order (the
            // sequence order), distinct from a struct's field-name order.
            (Self::Composite(a), Self::Composite(b)) => a.cmp(b),
            (Self::Set(a), Self::Set(b)) => a.cmp(b),
            (Self::Map(a), Self::Map(b)) => a.cmp(b),
            (Self::None, Self::None) => Ordering::Equal,
            _ => self.rank().cmp(&other.rank()),
        }
    }
}
