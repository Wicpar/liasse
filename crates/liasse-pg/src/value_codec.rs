//! Lossless, self-describing wire codec for [`Value`].
//!
//! The store is schema-free: unlike the runtime it never holds a [`Type`], so it
//! cannot decode a value's *canonical* wire form (which is type-directed —
//! `"1"` is an `int`, a `decimal`, or a `timestamp` count depending on the
//! declared column) back into a typed [`Value`]. Persisting to JSONB therefore
//! needs a serialization that carries its own type tag.
//!
//! Every value becomes a single-key JSON object `{"<tag>": payload}`. The tag
//! names the variant; the payload uses that variant's own canonical text (which
//! *is* lossless once the type is known) so the round-trip reproduces the exact
//! value, incarnation-for-incarnation. Big integers (`int`, `timestamp` counts,
//! `duration` nanoseconds) travel as strings so no JSON number ever rounds them.
//!
//! The codec is built strictly from `liasse-value`'s public parse/render surface
//! — this crate never reaches into that crate's internals — so a decoded value
//! is as well-formed as one the runtime parsed. A malformed durable record is a
//! [`StoreError::Corruption`]: it is never a well-formed input's fault.

use std::collections::BTreeMap;

use liasse_value::{
    BlobDescriptor, Bytes, CalendarPeriodBuilder, Date, Decimal, Duration, EnumValue, Integer,
    Json, MediaType, Period, Precision, Ref, RefKey, Sha512, Struct, Text, Timestamp, Uuid, Value,
};
use liasse_store::StoreError;
use serde_json::{Map, Value as J};

/// Encode a key component in its Annex B canonical form.
///
/// A key's identity is Annex B numeric order (B.1), which is *coarser* than a
/// value's exact wire incarnation. Two axes make Annex-B-equal values render to
/// distinct wire text: a `decimal`'s scale (`1.0` and `1.00` are one key value)
/// and a `timestamp`'s precision (counts that normalize to the same instant are
/// one key value). The value column preserves those (incarnation-for-incarnation,
/// per [`encode`]), but a durable *address key* must collapse Annex-B-equal
/// values to one identity, or a committed update/delete addressed by a
/// scale/precision-variant key targets a different durable row than the insert —
/// diverging from the in-memory reference, whose [`RowAddress`] `Ord` is Annex B.
///
/// The canonicalization recurses, since a composite or `ref` key can nest a
/// decimal or timestamp, then delegates to [`encode`] so the result stays a
/// well-formed, decodable wire value.
///
/// [`RowAddress`]: liasse_store::RowAddress
#[must_use]
pub fn encode_key(value: &Value) -> J {
    encode(&canonical_key(value))
}

/// Map a key value to its Annex B canonical representative: decimals lose their
/// trailing-zero scale, timestamps reduce to the coarsest exact precision, and
/// composite/nested keys are rewritten component-wise. Every other value is its
/// own representative.
///
/// Exposed to the crate so the order-preserving `key_enc` codec can share the
/// exact same Annex-B canonicalization the JSONB `key_wire` column uses, keeping
/// the two durable key columns in lock-step.
pub(crate) fn canonical_key(value: &Value) -> Value {
    match value {
        Value::Decimal(d) => {
            Value::Decimal(Decimal::from_big_decimal(d.as_big_decimal().normalized()))
        }
        Value::Timestamp(ts) => Value::Timestamp(canonical_timestamp(*ts)),
        Value::Ref(r) => Value::Ref(canonical_ref(r)),
        Value::Struct(s) => Value::Struct(Struct::new(
            s.fields().map(|(name, field)| (name.clone(), canonical_key(field))),
        )),
        Value::Composite(components) => {
            Value::Composite(components.iter().map(canonical_key).collect())
        }
        Value::Set(members) => Value::Set(members.iter().map(canonical_key).collect()),
        Value::Map(entries) => Value::Map(
            entries.iter().map(|(k, v)| (canonical_key(k), canonical_key(v))).collect(),
        ),
        other => other.clone(),
    }
}

/// Reduce a timestamp to the coarsest precision that represents it exactly, so
/// that every count/precision pair denoting the same instant (B.1 "signed order
/// after exact precision normalization") maps to a single representative — e.g.
/// `1000` millis and `1` second both become `(1, seconds)`.
fn canonical_timestamp(ts: Timestamp) -> Timestamp {
    let Ok(mut count) = ts.to_canonical_text().parse::<i128>() else {
        return ts;
    };
    let mut precision = ts.precision();
    while let Some(coarser) = coarser_precision(precision) {
        if count % 1000 != 0 {
            break;
        }
        count /= 1000;
        precision = coarser;
    }
    Timestamp::new(count, precision)
}

/// The next-coarser precision (each step is a factor of 1000), or `None` at the
/// coarsest (`seconds`).
fn coarser_precision(precision: Precision) -> Option<Precision> {
    match precision {
        Precision::Nanos => Some(Precision::Micros),
        Precision::Micros => Some(Precision::Millis),
        Precision::Millis => Some(Precision::Seconds),
        Precision::Seconds => None,
    }
}

/// Canonicalize the key components a `ref` carries as its target key.
fn canonical_ref(reference: &Ref) -> Ref {
    match reference.key() {
        RefKey::Scalar(value) => Ref::scalar(canonical_key(value)),
        RefKey::Composite(components) => {
            Ref::composite(components.iter().map(canonical_key).collect())
        }
    }
}

/// Encode a [`Value`] into its tagged, self-describing wire form.
#[must_use]
pub fn encode(value: &Value) -> J {
    match value {
        Value::Text(t) => tag("s", J::String(t.as_str().to_owned())),
        Value::Bool(b) => tag("b", J::Bool(*b)),
        Value::Int(i) => tag("i", J::String(i.to_canonical_text())),
        Value::Decimal(d) => tag("d", J::String(d.to_canonical_text())),
        Value::Bytes(b) => tag("y", J::String(b.to_base64())),
        Value::Uuid(u) => tag("u", J::String(u.to_canonical_text())),
        Value::Date(d) => tag("date", J::String(d.to_canonical_text())),
        Value::Timestamp(ts) => tag(
            "ts",
            J::Array(vec![
                J::String(ts.to_canonical_text()),
                J::String(ts.precision().keyword().to_owned()),
            ]),
        ),
        Value::Duration(d) => tag("dur", J::String(d.as_nanos().to_string())),
        Value::Period(p) => tag("per", encode_period(p)),
        Value::Json(j) => tag("j", j.to_wire()),
        Value::Blob(b) => tag("blob", encode_blob(b)),
        Value::Enum(e) => tag(
            "enum",
            J::Array(vec![J::from(e.ordinal()), J::String(e.label().to_owned())]),
        ),
        Value::Ref(r) => tag("ref", encode_ref(r)),
        Value::Struct(s) => tag("st", encode_struct(s)),
        Value::Composite(components) => {
            tag("comp", J::Array(components.iter().map(encode).collect()))
        }
        Value::Set(members) => tag("set", J::Array(members.iter().map(encode).collect())),
        Value::Map(entries) => tag(
            "map",
            J::Array(
                entries
                    .iter()
                    .map(|(k, v)| J::Array(vec![encode(k), encode(v)]))
                    .collect(),
            ),
        ),
        Value::None => tag("none", J::Bool(true)),
    }
}

/// Decode a tagged wire value produced by [`encode`] back into a [`Value`].
pub fn decode(wire: &J) -> Result<Value, StoreError> {
    let (tag, payload) = single_member(wire)?;
    match tag {
        "s" => Ok(Value::Text(Text::new(as_str(payload)?))),
        "b" => Ok(Value::Bool(as_bool(payload)?)),
        "i" => Integer::parse(as_str(payload)?).map(Value::Int).map_err(malformed),
        "d" => Decimal::parse(as_str(payload)?).map(Value::Decimal).map_err(malformed),
        "y" => Bytes::from_base64(as_str(payload)?).map(Value::Bytes).map_err(malformed),
        "u" => Uuid::parse(as_str(payload)?).map(Value::Uuid).map_err(malformed),
        "date" => Date::parse(as_str(payload)?).map(Value::Date).map_err(malformed),
        "ts" => decode_timestamp(payload),
        "dur" => decode_duration(payload),
        "per" => decode_period(payload).map(|p| Value::Period(Box::new(p))),
        "j" => Json::from_wire(payload).map(Value::Json).map_err(malformed),
        "blob" => decode_blob(payload).map(|b| Value::Blob(Box::new(b))),
        "enum" => decode_enum(payload),
        "ref" => decode_ref(payload).map(Value::Ref),
        "st" => decode_struct(payload),
        "comp" => decode_seq(payload).map(Value::Composite),
        // A.1 / SPEC-ISSUES item 29: `none` is never a set member (a composite,
        // in contrast, MAY carry a positional `none`, so the drop is set-only and
        // not in `decode_seq`). After the model rejects an `optional` element type
        // no stored set holds a `none`; the filter guards a legacy/corrupt row.
        "set" => decode_seq(payload)
            .map(|v| Value::Set(v.into_iter().filter(|m| !matches!(m, Value::None)).collect())),
        "map" => decode_map(payload),
        "none" => Ok(Value::None),
        other => Err(corrupt(format!("unknown value tag `{other}`"))),
    }
}

fn encode_period(period: &Period) -> J {
    match period {
        Period::Fixed(d) => J::Array(vec![J::String("f".to_owned()), J::String(d.as_nanos().to_string())]),
        Period::Calendar(c) => {
            let (years, months, weeks, days) = c.calendar_magnitudes();
            let (overflow, ambiguous, missing) = c.policy_keywords();
            let mut obj = Map::new();
            obj.insert("years".to_owned(), J::from(years));
            obj.insert("months".to_owned(), J::from(months));
            obj.insert("weeks".to_owned(), J::from(weeks));
            obj.insert("days".to_owned(), J::from(days));
            obj.insert("time".to_owned(), J::String(c.time().as_nanos().to_string()));
            obj.insert("overflow".to_owned(), J::String(overflow.to_owned()));
            obj.insert("ambiguous".to_owned(), J::String(ambiguous.to_owned()));
            obj.insert("missing".to_owned(), J::String(missing.to_owned()));
            if let Some(zone) = c.zone() {
                obj.insert("zone".to_owned(), J::String(zone.to_owned()));
            }
            J::Array(vec![J::String("c".to_owned()), J::Object(obj)])
        }
    }
}

fn decode_period(payload: &J) -> Result<Period, StoreError> {
    let items = as_array(payload)?;
    let kind = items.first().and_then(J::as_str).ok_or_else(|| corrupt("period missing kind"))?;
    let body = items.get(1).ok_or_else(|| corrupt("period missing body"))?;
    match kind {
        "f" => Ok(Period::Fixed(Duration::from_nanos(as_i128(body)?))),
        "c" => {
            let obj = as_object(body)?;
            let mut builder = CalendarPeriodBuilder {
                years: member_i64(obj, "years")?,
                months: member_i64(obj, "months")?,
                weeks: member_i64(obj, "weeks")?,
                days: member_i64(obj, "days")?,
                time: Duration::from_nanos(as_i128(member(obj, "time")?)?),
                zone: obj.get("zone").and_then(J::as_str).map(str::to_owned),
                ..CalendarPeriodBuilder::default()
            };
            builder.set_overflow(member_str(obj, "overflow")?).map_err(malformed)?;
            builder.set_ambiguous(member_str(obj, "ambiguous")?).map_err(malformed)?;
            builder.set_missing(member_str(obj, "missing")?).map_err(malformed)?;
            builder.build().map(Period::Calendar).map_err(malformed)
        }
        other => Err(corrupt(format!("unknown period kind `{other}`"))),
    }
}

fn encode_blob(descriptor: &BlobDescriptor) -> J {
    let mut obj = Map::new();
    obj.insert("sha512".to_owned(), J::String(descriptor.sha512().to_canonical_text()));
    obj.insert("bytes".to_owned(), J::String(descriptor.byte_count().to_string()));
    obj.insert("media".to_owned(), J::String(descriptor.media().as_str().to_owned()));
    if let Some(name) = descriptor.name() {
        obj.insert("name".to_owned(), J::String(name.to_owned()));
    }
    J::Object(obj)
}

fn decode_blob(payload: &J) -> Result<BlobDescriptor, StoreError> {
    let obj = as_object(payload)?;
    let sha512 = Sha512::parse(member_str(obj, "sha512")?).map_err(malformed)?;
    let bytes: u64 = member_str(obj, "bytes")?
        .parse()
        .map_err(|_| corrupt("blob byte count is not a u64"))?;
    let media = MediaType::new(member_str(obj, "media")?);
    let name = obj.get("name").and_then(J::as_str).map(str::to_owned);
    Ok(BlobDescriptor::new(sha512, bytes, media, name))
}

fn decode_enum(payload: &J) -> Result<Value, StoreError> {
    let items = as_array(payload)?;
    let ordinal = items
        .first()
        .and_then(J::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| corrupt("enum ordinal is not a u32"))?;
    let label = items.get(1).and_then(J::as_str).ok_or_else(|| corrupt("enum missing label"))?;
    // An `EnumValue` *is* its recorded `(ordinal, label)` pair — the pair on which
    // its equality and Annex B order are defined — so rebuild it directly from the
    // two parts. Reconstructing through a fabricated declaration would let a label
    // that is itself a synthetic placeholder collide, and a label is arbitrary A.1
    // text (`U+0000` included), so no synthetic-declaration scheme is collision-free.
    Ok(Value::Enum(EnumValue::from_parts(ordinal, label)))
}

fn encode_ref(reference: &Ref) -> J {
    let mut obj = Map::new();
    match reference.key() {
        RefKey::Scalar(value) => {
            obj.insert("scalar".to_owned(), encode(value));
        }
        RefKey::Composite(components) => {
            obj.insert("composite".to_owned(), J::Array(components.iter().map(encode).collect()));
        }
    }
    J::Object(obj)
}

fn decode_ref(payload: &J) -> Result<Ref, StoreError> {
    let obj = as_object(payload)?;
    if let Some(scalar) = obj.get("scalar") {
        return Ok(Ref::scalar(decode(scalar)?));
    }
    let composite = obj.get("composite").ok_or_else(|| corrupt("ref missing key"))?;
    Ok(Ref::composite(decode_seq(composite)?))
}

fn encode_struct(value: &Struct) -> J {
    J::Array(
        value
            .fields()
            .map(|(name, field)| J::Array(vec![J::String(name.as_str().to_owned()), encode(field)]))
            .collect(),
    )
}

fn decode_struct(payload: &J) -> Result<Value, StoreError> {
    let mut fields = Vec::new();
    for entry in as_array(payload)? {
        let pair = as_array(entry)?;
        let name = pair.first().and_then(J::as_str).ok_or_else(|| corrupt("struct field name"))?;
        let field = pair.get(1).ok_or_else(|| corrupt("struct field value"))?;
        fields.push((Text::new(name), decode(field)?));
    }
    Ok(Value::Struct(Struct::new(fields)))
}

fn decode_map(payload: &J) -> Result<Value, StoreError> {
    let mut entries: BTreeMap<Value, Value> = BTreeMap::new();
    for entry in as_array(payload)? {
        let pair = as_array(entry)?;
        let key = pair.first().ok_or_else(|| corrupt("map entry key"))?;
        let val = pair.get(1).ok_or_else(|| corrupt("map entry value"))?;
        // A.1: a map never stores a `none` value — the key is simply absent.
        // Mirrors `liasse-value::decode_map`; the model rejects an `optional`
        // value type, so this guards a legacy/corrupt row (memory/PG agree).
        let decoded = decode(val)?;
        if matches!(decoded, Value::None) {
            continue;
        }
        entries.insert(decode(key)?, decoded);
    }
    Ok(Value::Map(entries))
}

fn decode_timestamp(payload: &J) -> Result<Value, StoreError> {
    let items = as_array(payload)?;
    let count: i128 = items
        .first()
        .and_then(J::as_str)
        .ok_or_else(|| corrupt("timestamp missing count"))?
        .parse()
        .map_err(|_| corrupt("timestamp count is not an i128"))?;
    let keyword = items.get(1).and_then(J::as_str).ok_or_else(|| corrupt("timestamp precision"))?;
    let precision = Precision::parse(keyword).ok_or_else(|| corrupt("unknown timestamp precision"))?;
    Ok(Value::Timestamp(Timestamp::new(count, precision)))
}

fn decode_duration(payload: &J) -> Result<Value, StoreError> {
    Ok(Value::Duration(Duration::from_nanos(as_i128(payload)?)))
}

fn decode_seq(payload: &J) -> Result<Vec<Value>, StoreError> {
    as_array(payload)?.iter().map(decode).collect()
}

/// Encode a row's recorded admission instant (§14.1 `$created`, §22.6) into the
/// `nodes.created`/`commit_log.created` JSONB — the exact, precision-preserving
/// `ts` wire form [`encode`] gives a `timestamp`, so a decode round-trips the
/// engine clock's instant byte-for-byte and the two backends record it identically.
#[must_use]
pub fn encode_created(created: Timestamp) -> J {
    encode(&Value::Timestamp(created))
}

/// Decode a `created` column produced by [`encode_created`] back into a
/// [`Timestamp`]. A column carrying any other tagged value is a durable corruption.
pub fn decode_created(wire: &J) -> Result<Timestamp, StoreError> {
    match decode(wire)? {
        Value::Timestamp(ts) => Ok(ts),
        _ => Err(corrupt("created column is not a timestamp")),
    }
}

fn tag(name: &str, payload: J) -> J {
    let mut obj = Map::new();
    obj.insert(name.to_owned(), payload);
    J::Object(obj)
}

fn single_member(wire: &J) -> Result<(&str, &J), StoreError> {
    let obj = as_object(wire)?;
    let mut iter = obj.iter();
    match (iter.next(), iter.next()) {
        (Some((tag, payload)), None) => Ok((tag.as_str(), payload)),
        _ => Err(corrupt("a tagged value must be a single-member object")),
    }
}

fn as_object(wire: &J) -> Result<&Map<String, J>, StoreError> {
    wire.as_object().ok_or_else(|| corrupt("expected a JSON object"))
}

fn as_array(wire: &J) -> Result<&Vec<J>, StoreError> {
    wire.as_array().ok_or_else(|| corrupt("expected a JSON array"))
}

fn as_str(wire: &J) -> Result<&str, StoreError> {
    wire.as_str().ok_or_else(|| corrupt("expected a JSON string"))
}

fn as_bool(wire: &J) -> Result<bool, StoreError> {
    wire.as_bool().ok_or_else(|| corrupt("expected a JSON bool"))
}

fn as_i128(wire: &J) -> Result<i128, StoreError> {
    as_str(wire)?.parse().map_err(|_| corrupt("expected an i128 string"))
}

fn member<'a>(obj: &'a Map<String, J>, key: &str) -> Result<&'a J, StoreError> {
    obj.get(key).ok_or_else(|| corrupt(format!("missing member `{key}`")))
}

fn member_str<'a>(obj: &'a Map<String, J>, key: &str) -> Result<&'a str, StoreError> {
    as_str(member(obj, key)?)
}

fn member_i64(obj: &Map<String, J>, key: &str) -> Result<i64, StoreError> {
    member(obj, key)?.as_i64().ok_or_else(|| corrupt(format!("member `{key}` is not an i64")))
}

fn corrupt(detail: impl Into<String>) -> StoreError {
    StoreError::Corruption { detail: detail.into() }
}

fn malformed<E: core::fmt::Display>(error: E) -> StoreError {
    StoreError::Corruption { detail: format!("malformed durable value: {error}") }
}
