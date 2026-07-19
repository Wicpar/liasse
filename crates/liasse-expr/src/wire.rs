//! The `eval-wire` postcard codec (§7.3/§7.4 of `liasse-pg/DESIGN-pure-pg.md`).
//!
//! An **internal, version-locked** wire — producer (the runtime) and consumer (the
//! pushdown extension) are required to be the same build (§7.7 handshake), so no
//! cross-version stability is promised and the derives impose no public-format
//! obligation. It exists so a lowered program's residual [`TypedExpr`]s and hoisted
//! [`Cell`] env can cross into the database and evaluate through the same
//! interpreter.
//!
//! Serialization is `postcard` over feature-gated `serde` derives on the closed
//! typed tree. [`Value`] and [`Type`] carry no derives of their own (they live in
//! `liasse-value`), so they travel through the self-describing [`WireValue`] and
//! [`WireType`] mirrors here, built strictly from `liasse-value`'s public
//! parse/render surface — a decoded value is as well-formed as one the runtime
//! parsed. A [`CallSite`](crate::env::CallSite) cannot round-trip (its `SourceId`
//! has no reconstruction), but `uuid()` is candidate-free and always hoisted, so no
//! residual ever carries a [`TypedKind::Uuid`](crate::typed::TypedKind); the wire
//! rejects one loudly rather than guessing.

use liasse_diag::ByteSpan;
use liasse_value::{
    BlobDescriptor, Bytes, CalendarPeriodBuilder, Date, Decimal, Duration, EnumValue, Integer,
    Json, MediaType, Period, Precision, Ref, RefKey, Sha512, Struct, Text, Timestamp, Uuid, Value,
};
use serde::{Deserialize, Serialize};

use crate::env::Cell;
use crate::typed::TypedExpr;

/// A failure encoding to or decoding from the eval wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// The `postcard` layer failed (a malformed or truncated blob).
    Codec(String),
    /// A decoded scalar did not parse back into its `liasse-value` type — a
    /// corrupt or version-skewed blob (never a well-formed producer's fault).
    Malformed(String),
    /// The tree carried a `uuid()` node, which the wire cannot represent (its call
    /// site has no reconstruction). Unreachable for a lowered residual: `uuid()` is
    /// candidate-free and always hoisted.
    Uuid,
}

impl core::fmt::Display for WireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Codec(detail) => write!(f, "eval-wire codec error: {detail}"),
            Self::Malformed(detail) => write!(f, "eval-wire malformed value: {detail}"),
            Self::Uuid => write!(f, "eval-wire cannot carry a `uuid()` node (it is always hoisted)"),
        }
    }
}

impl std::error::Error for WireError {}

/// Serialize a residual [`TypedExpr`] to its version-locked wire bytes.
pub fn to_wire(expr: &TypedExpr) -> Result<Vec<u8>, WireError> {
    postcard::to_allocvec(expr).map_err(|error| serialize_error(&error))
}

/// Deserialize a residual [`TypedExpr`] from its wire bytes.
pub fn from_wire(bytes: &[u8]) -> Result<TypedExpr, WireError> {
    postcard::from_bytes(bytes).map_err(|error| WireError::Codec(error.to_string()))
}

/// Serialize a hoisted env — synthetic-name → [`Cell`] entries — to wire bytes.
pub fn env_to_wire(env: &[(String, Cell)]) -> Result<Vec<u8>, WireError> {
    postcard::to_allocvec(env).map_err(|error| serialize_error(&error))
}

/// Deserialize a hoisted env from wire bytes.
pub fn env_from_wire(bytes: &[u8]) -> Result<Vec<(String, Cell)>, WireError> {
    postcard::from_bytes(bytes).map_err(|error| WireError::Codec(error.to_string()))
}

/// Map a postcard serialize error, surfacing the deliberate `uuid()` rejection.
fn serialize_error(error: &postcard::Error) -> WireError {
    let text = error.to_string();
    if text.contains(UUID_SENTINEL) {
        WireError::Uuid
    } else {
        WireError::Codec(text)
    }
}

/// The marker a `uuid()` serialize attempt raises through serde's custom-error
/// channel, recovered as [`WireError::Uuid`].
const UUID_SENTINEL: &str = "liasse-eval-wire-uuid";

// --- serde `with` adapters for the foreign leaf types on the typed tree ---------

/// [`ByteSpan`] ↔ `(start, end)`. Spans are decorative for evaluation
/// ([`EvalError`](crate::EvalError) carries none), so a reversed pair on decode
/// collapses to a point rather than failing the whole blob.
pub(crate) mod byte_span_serde {
    use super::{ByteSpan, Deserialize, Serialize};

    pub(crate) fn serialize<S: serde::Serializer>(span: &ByteSpan, s: S) -> Result<S::Ok, S::Error> {
        (span.start(), span.end()).serialize(s)
    }

    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<ByteSpan, D::Error> {
        let (start, end) = <(u32, u32)>::deserialize(d)?;
        Ok(ByteSpan::new(start, end).unwrap_or_else(|| ByteSpan::point(start)))
    }
}

/// [`CallSite`](crate::env::CallSite) is unrepresentable (its `SourceId` cannot be
/// rebuilt); a `uuid()` node is always hoisted, so serialize raises the sentinel
/// and deserialize is never reached.
pub(crate) mod callsite_serde {
    use super::UUID_SENTINEL;
    use crate::env::CallSite;

    pub(crate) fn serialize<S: serde::Serializer>(_: &CallSite, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom(UUID_SENTINEL))
    }

    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(_: D) -> Result<CallSite, D::Error> {
        Err(serde::de::Error::custom("eval-wire cannot decode a uuid() call site"))
    }
}

/// [`Value`] ↔ [`WireValue`], for a `#[serde(with)]` field.
pub(crate) mod value_serde {
    use super::{Value, WireValue};

    pub(crate) fn serialize<S: serde::Serializer>(value: &Value, s: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&WireValue::from(value), s)
    }

    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
        let wire: WireValue = serde::Deserialize::deserialize(d)?;
        wire.into_value().map_err(serde::de::Error::custom)
    }
}

/// `Vec<Value>` ↔ `Vec<WireValue>`, for the sort-tuple field on a [`Cell`]'s row.
pub(crate) mod value_vec_serde {
    use super::{Value, WireValue};

    pub(crate) fn serialize<S: serde::Serializer>(values: &[Value], s: S) -> Result<S::Ok, S::Error> {
        let wire: Vec<WireValue> = values.iter().map(WireValue::from).collect();
        serde::Serialize::serialize(&wire, s)
    }

    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Value>, D::Error> {
        let wire: Vec<WireValue> = serde::Deserialize::deserialize(d)?;
        wire.into_iter().map(WireValue::into_value).collect::<Result<_, _>>().map_err(serde::de::Error::custom)
    }
}

/// [`Type`] ↔ [`WireType`], for a `#[serde(with)]` field.
pub(crate) mod type_serde {
    use liasse_value::Type;

    use super::WireType;

    pub(crate) fn serialize<S: serde::Serializer>(ty: &Type, s: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&WireType::from(ty), s)
    }

    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Type, D::Error> {
        let wire: WireType = serde::Deserialize::deserialize(d)?;
        wire.into_type().map_err(serde::de::Error::custom)
    }
}

// --- the self-describing Value mirror -------------------------------------------

/// A `postcard`-friendly mirror of [`Value`], every variant reconstructible from
/// `liasse-value`'s public parse/render surface. Big numbers travel as their
/// canonical text so no wire number ever rounds them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum WireValue {
    Text(String),
    Bool(bool),
    Int(String),
    Decimal(String),
    Bytes(String),
    Uuid(String),
    Date(String),
    Timestamp(String, String),
    Duration(String),
    PeriodFixed(String),
    PeriodCalendar(WireCalendar),
    Json(String),
    Blob { sha512: String, bytes: u64, media: String, name: Option<String> },
    Enum { ordinal: u32, label: String },
    RefScalar(Box<WireValue>),
    RefComposite(Vec<WireValue>),
    Struct(Vec<(String, WireValue)>),
    Composite(Vec<WireValue>),
    Set(Vec<WireValue>),
    Map(Vec<(WireValue, WireValue)>),
    None,
}

/// The calendar-period fields (§14.7), reconstructed via [`CalendarPeriodBuilder`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WireCalendar {
    years: i64,
    months: i64,
    weeks: i64,
    days: i64,
    time: String,
    zone: Option<String>,
    overflow: String,
    ambiguous: String,
    missing: String,
}

impl From<&Value> for WireValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Text(t) => Self::Text(t.as_str().to_owned()),
            Value::Bool(b) => Self::Bool(*b),
            Value::Int(i) => Self::Int(i.to_canonical_text()),
            Value::Decimal(d) => Self::Decimal(d.to_canonical_text()),
            Value::Bytes(b) => Self::Bytes(b.to_base64()),
            Value::Uuid(u) => Self::Uuid(u.to_canonical_text()),
            Value::Date(d) => Self::Date(d.to_canonical_text()),
            Value::Timestamp(ts) => {
                Self::Timestamp(ts.to_canonical_text(), ts.precision().keyword().to_owned())
            }
            Value::Duration(d) => Self::Duration(d.as_nanos().to_string()),
            Value::Period(p) => match p.as_ref() {
                Period::Fixed(d) => Self::PeriodFixed(d.as_nanos().to_string()),
                Period::Calendar(c) => Self::PeriodCalendar(calendar_to_wire(c)),
            },
            Value::Json(j) => Self::Json(serde_json::to_string(&j.to_wire()).unwrap_or_default()),
            Value::Blob(b) => Self::Blob {
                sha512: b.sha512().to_canonical_text(),
                bytes: b.byte_count(),
                media: b.media().as_str().to_owned(),
                name: b.name().map(str::to_owned),
            },
            Value::Enum(e) => Self::Enum { ordinal: e.ordinal(), label: e.label().to_owned() },
            Value::Ref(r) => match r.key() {
                RefKey::Scalar(inner) => Self::RefScalar(Box::new(WireValue::from(inner.as_ref()))),
                RefKey::Composite(components) => {
                    Self::RefComposite(components.iter().map(WireValue::from).collect())
                }
            },
            Value::Struct(s) => Self::Struct(
                s.fields().map(|(name, field)| (name.as_str().to_owned(), WireValue::from(field))).collect(),
            ),
            Value::Composite(components) => {
                Self::Composite(components.iter().map(WireValue::from).collect())
            }
            Value::Set(members) => Self::Set(members.iter().map(WireValue::from).collect()),
            Value::Map(entries) => Self::Map(
                entries.iter().map(|(k, v)| (WireValue::from(k), WireValue::from(v))).collect(),
            ),
            Value::None => Self::None,
        }
    }
}

impl WireValue {
    fn into_value(self) -> Result<Value, String> {
        Ok(match self {
            Self::Text(t) => Value::Text(Text::new(t)),
            Self::Bool(b) => Value::Bool(b),
            Self::Int(s) => Value::Int(Integer::parse(&s).map_err(err)?),
            Self::Decimal(s) => Value::Decimal(Decimal::parse(&s).map_err(err)?),
            Self::Bytes(s) => Value::Bytes(Bytes::from_base64(&s).map_err(err)?),
            Self::Uuid(s) => Value::Uuid(Uuid::parse(&s).map_err(err)?),
            Self::Date(s) => Value::Date(Date::parse(&s).map_err(err)?),
            Self::Timestamp(count, keyword) => {
                let precision = Precision::parse(&keyword).ok_or("unknown timestamp precision")?;
                let count: i128 = count.parse().map_err(|_| "timestamp count is not an i128")?;
                Value::Timestamp(Timestamp::new(count, precision))
            }
            Self::Duration(s) => {
                Value::Duration(Duration::from_nanos(s.parse().map_err(|_| "duration is not an i128")?))
            }
            Self::PeriodFixed(s) => Value::Period(Box::new(Period::Fixed(Duration::from_nanos(
                s.parse().map_err(|_| "period nanos is not an i128")?,
            )))),
            Self::PeriodCalendar(c) => Value::Period(Box::new(Period::Calendar(calendar_from_wire(c)?))),
            Self::Json(s) => {
                let wire: serde_json::Value = serde_json::from_str(&s).map_err(err)?;
                Value::Json(Json::from_wire(&wire).map_err(err)?)
            }
            Self::Blob { sha512, bytes, media, name } => Value::Blob(Box::new(BlobDescriptor::new(
                Sha512::parse(&sha512).map_err(err)?,
                bytes,
                MediaType::new(media),
                name,
            ))),
            Self::Enum { ordinal, label } => Value::Enum(EnumValue::from_parts(ordinal, label)),
            Self::RefScalar(inner) => Value::Ref(Ref::scalar(inner.into_value()?)),
            Self::RefComposite(components) => Value::Ref(Ref::composite(into_values(components)?)),
            Self::Struct(fields) => Value::Struct(Struct::new(
                fields
                    .into_iter()
                    .map(|(name, field)| Ok((Text::new(name), field.into_value()?)))
                    .collect::<Result<Vec<_>, String>>()?,
            )),
            Self::Composite(components) => Value::Composite(into_values(components)?),
            Self::Set(members) => Value::Set(into_values(members)?.into_iter().collect()),
            Self::Map(entries) => Value::Map(
                entries
                    .into_iter()
                    .map(|(k, v)| Ok((k.into_value()?, v.into_value()?)))
                    .collect::<Result<_, String>>()?,
            ),
            Self::None => Value::None,
        })
    }
}

fn into_values(wire: Vec<WireValue>) -> Result<Vec<Value>, String> {
    wire.into_iter().map(WireValue::into_value).collect()
}

fn calendar_to_wire(calendar: &liasse_value::CalendarPeriod) -> WireCalendar {
    let (years, months, weeks, days) = calendar.calendar_magnitudes();
    let (overflow, ambiguous, missing) = calendar.policy_keywords();
    WireCalendar {
        years,
        months,
        weeks,
        days,
        time: calendar.time().as_nanos().to_string(),
        zone: calendar.zone().map(str::to_owned),
        overflow: overflow.to_owned(),
        ambiguous: ambiguous.to_owned(),
        missing: missing.to_owned(),
    }
}

fn calendar_from_wire(c: WireCalendar) -> Result<liasse_value::CalendarPeriod, String> {
    let mut builder = CalendarPeriodBuilder {
        years: c.years,
        months: c.months,
        weeks: c.weeks,
        days: c.days,
        time: Duration::from_nanos(c.time.parse().map_err(|_| "calendar time is not an i128")?),
        zone: c.zone,
        ..CalendarPeriodBuilder::default()
    };
    builder.set_overflow(&c.overflow).map_err(err)?;
    builder.set_ambiguous(&c.ambiguous).map_err(err)?;
    builder.set_missing(&c.missing).map_err(err)?;
    builder.build().map_err(err)
}

// --- the self-describing Type mirror --------------------------------------------

/// A `postcard`-friendly mirror of [`Type`](liasse_value::Type).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum WireType {
    Text,
    Bool,
    Int,
    Decimal,
    Bytes,
    Uuid,
    Date,
    Timestamp(String),
    Duration,
    Period,
    Json,
    Blob,
    Enum(Vec<String>),
    Optional(Box<WireType>),
    Set(Box<WireType>),
    Map(Box<WireType>, Box<WireType>),
    View(Box<WireType>),
    RefScalar(Box<WireType>),
    RefComposite(Vec<(String, WireType)>),
    Struct(Vec<(String, WireType)>),
    Composite(Vec<(String, WireType)>),
}

impl From<&liasse_value::Type> for WireType {
    fn from(ty: &liasse_value::Type) -> Self {
        use liasse_value::{RefTarget, Type};
        match ty {
            Type::Text => Self::Text,
            Type::Bool => Self::Bool,
            Type::Int => Self::Int,
            Type::Decimal => Self::Decimal,
            Type::Bytes => Self::Bytes,
            Type::Uuid => Self::Uuid,
            Type::Date => Self::Date,
            Type::Timestamp(precision) => Self::Timestamp(precision.keyword().to_owned()),
            Type::Duration => Self::Duration,
            Type::Period => Self::Period,
            Type::Json => Self::Json,
            Type::Blob => Self::Blob,
            Type::Enum(e) => Self::Enum(e.labels().to_vec()),
            Type::Optional(inner) => Self::Optional(Box::new(WireType::from(inner.as_ref()))),
            Type::Set(inner) => Self::Set(Box::new(WireType::from(inner.as_ref()))),
            Type::Map(k, v) => {
                Self::Map(Box::new(WireType::from(k.as_ref())), Box::new(WireType::from(v.as_ref())))
            }
            Type::View(inner) => Self::View(Box::new(WireType::from(inner.as_ref()))),
            Type::Ref(RefTarget::Scalar(inner)) => Self::RefScalar(Box::new(WireType::from(inner.as_ref()))),
            Type::Ref(RefTarget::Composite(components)) => Self::RefComposite(components_to_wire(components)),
            Type::Struct(s) => {
                Self::Struct(s.fields().map(|(name, ty)| (name.clone(), WireType::from(ty))).collect())
            }
            Type::Composite(components) => Self::Composite(components_to_wire(components)),
        }
    }
}

impl WireType {
    fn into_type(self) -> Result<liasse_value::Type, String> {
        use liasse_value::{EnumType, RefTarget, StructType, Type};
        Ok(match self {
            Self::Text => Type::Text,
            Self::Bool => Type::Bool,
            Self::Int => Type::Int,
            Self::Decimal => Type::Decimal,
            Self::Bytes => Type::Bytes,
            Self::Uuid => Type::Uuid,
            Self::Date => Type::Date,
            Self::Timestamp(keyword) => {
                Type::Timestamp(Precision::parse(&keyword).ok_or("unknown timestamp precision")?)
            }
            Self::Duration => Type::Duration,
            Self::Period => Type::Period,
            Self::Json => Type::Json,
            Self::Blob => Type::Blob,
            Self::Enum(labels) => Type::Enum(EnumType::new(labels).map_err(err)?),
            Self::Optional(inner) => Type::Optional(Box::new(inner.into_type()?)),
            Self::Set(inner) => Type::Set(Box::new(inner.into_type()?)),
            Self::Map(k, v) => Type::Map(Box::new(k.into_type()?), Box::new(v.into_type()?)),
            Self::View(inner) => Type::View(Box::new(inner.into_type()?)),
            Self::RefScalar(inner) => Type::Ref(RefTarget::Scalar(Box::new(inner.into_type()?))),
            Self::RefComposite(components) => Type::Ref(RefTarget::Composite(components_from_wire(components)?)),
            Self::Struct(fields) => Type::Struct(StructType::new(components_from_wire(fields)?)),
            Self::Composite(components) => Type::Composite(components_from_wire(components)?),
        })
    }
}

fn components_to_wire(components: &[(String, liasse_value::Type)]) -> Vec<(String, WireType)> {
    components.iter().map(|(name, ty)| (name.clone(), WireType::from(ty))).collect()
}

fn components_from_wire(components: Vec<(String, WireType)>) -> Result<Vec<(String, liasse_value::Type)>, String> {
    components.into_iter().map(|(name, ty)| Ok((name, ty.into_type()?))).collect()
}

fn err<E: core::fmt::Display>(error: E) -> String {
    error.to_string()
}
