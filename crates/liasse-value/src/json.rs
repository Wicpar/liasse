//! `json` — canonical schema-free JSON value (A.7). Ordering per B.3.

use core::cmp::Ordering;
use std::collections::BTreeMap;

use bigdecimal::BigDecimal;

use crate::decimal::Decimal;
use crate::error::{JsonShape, ValueError};

/// A canonical JSON value.
///
/// `null` is a value of `json` (A.7) and is distinct from the Liasse `none`.
/// Object keys are held in a [`BTreeMap`], whose `String` ordering is text
/// order (A.7), so members are always canonically ordered. Numbers are exact
/// [`BigDecimal`]s and compare mathematically (B.3).
#[derive(Debug, Clone)]
pub enum Json {
    Null,
    Bool(bool),
    Number(BigDecimal),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    /// The B.3 type rank: `null < bool < number < string < array < object`.
    const fn rank(&self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Bool(_) => 1,
            Self::Number(_) => 2,
            Self::String(_) => 3,
            Self::Array(_) => 4,
            Self::Object(_) => 5,
        }
    }

    /// Decode a `serde_json` value into canonical form (recursively sorting
    /// object keys and preserving arbitrary-precision numbers).
    pub fn from_wire(value: &serde_json::Value) -> Result<Self, ValueError> {
        match value {
            serde_json::Value::Null => Ok(Self::Null),
            serde_json::Value::Bool(b) => Ok(Self::Bool(*b)),
            serde_json::Value::Number(n) => {
                let text = n.to_string();
                let decimal = text
                    .parse::<BigDecimal>()
                    .map_err(|_| ValueError::MalformedDecimal(text.clone()))?;
                Ok(Self::Number(decimal))
            }
            serde_json::Value::String(s) => Ok(Self::String(s.clone())),
            serde_json::Value::Array(items) => items
                .iter()
                .map(Self::from_wire)
                .collect::<Result<Vec<_>, _>>()
                .map(Self::Array),
            serde_json::Value::Object(members) => {
                let mut map = BTreeMap::new();
                for (key, member) in members {
                    map.insert(key.clone(), Self::from_wire(member)?);
                }
                Ok(Self::Object(map))
            }
        }
    }

    /// Encode to a canonical `serde_json` value: object keys sorted (the
    /// `BTreeMap` already holds them so), `null` preserved, numbers in
    /// normalized plain spelling.
    #[must_use]
    pub fn to_wire(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(b) => serde_json::Value::Bool(*b),
            Self::Number(n) => {
                let text = Decimal::from_big_decimal(n.normalized()).to_canonical_text();
                text.parse::<serde_json::Value>()
                    .unwrap_or(serde_json::Value::String(text))
            }
            Self::String(s) => serde_json::Value::String(s.clone()),
            Self::Array(items) => {
                serde_json::Value::Array(items.iter().map(Self::to_wire).collect())
            }
            Self::Object(members) => {
                let mut map = serde_json::Map::new();
                for (key, member) in members {
                    map.insert(key.clone(), member.to_wire());
                }
                serde_json::Value::Object(map)
            }
        }
    }

    /// The JSON shape, for diagnostics.
    #[must_use]
    pub const fn shape(&self) -> JsonShape {
        match self {
            Self::Null => JsonShape::Null,
            Self::Bool(_) => JsonShape::Bool,
            Self::Number(_) => JsonShape::Number,
            Self::String(_) => JsonShape::String,
            Self::Array(_) => JsonShape::Array,
            Self::Object(_) => JsonShape::Object,
        }
    }
}

impl PartialEq for Json {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Json {}

impl PartialOrd for Json {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Json {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Null, Self::Null) => Ordering::Equal,
            (Self::Bool(a), Self::Bool(b)) => a.cmp(b),
            // B.3: numbers compare mathematically (scale-insensitive).
            (Self::Number(a), Self::Number(b)) => a.cmp(b),
            (Self::String(a), Self::String(b)) => a.cmp(b),
            // Vec: lexicographic, shorter after a shared prefix (B.3).
            (Self::Array(a), Self::Array(b)) => a.cmp(b),
            // BTreeMap: keys sorted, then lexicographic (key, value) pairs (B.3).
            (Self::Object(a), Self::Object(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}
