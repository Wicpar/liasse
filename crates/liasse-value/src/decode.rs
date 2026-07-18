//! Decoding strict-JSON wire values against a [`Type`] into canonical
//! [`Value`]s (Annex A). A successful decode is proof of well-formedness.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as J;

use crate::blob::{BlobDescriptor, MediaType, Sha512};
use crate::decimal::Decimal;
use crate::duration::Duration;
use crate::error::{JsonShape, ValueError};
use crate::int::Integer;
use crate::json::Json;
use crate::period::{CalendarPeriodBuilder, Period};
use crate::scalars::{Bytes, Text, Uuid};
use crate::temporal::{Date, Timestamp};
use crate::ty::{RefTarget, StructType, Type};
use crate::value::{Ref, Struct, Value};

impl Type {
    /// Decode a strict-JSON wire value against this type.
    pub fn decode(&self, wire: &J) -> Result<Value, ValueError> {
        match self {
            Type::Text => Ok(Value::Text(Text::new(self.expect_string(wire)?.to_owned()))),
            Type::Bool => Ok(Value::Bool(self.expect_bool(wire)?)),
            Type::Int => Ok(Value::Int(Integer::parse(&self.numeric_text(wire)?)?)),
            Type::Decimal => Ok(Value::Decimal(Decimal::parse(&self.numeric_text(wire)?)?)),
            Type::Bytes => self.decode_bytes(wire),
            Type::Uuid => Ok(Value::Uuid(Uuid::parse(self.expect_string(wire)?)?)),
            Type::Date => Ok(Value::Date(Date::parse(self.expect_string(wire)?)?)),
            Type::Timestamp(precision) => Ok(Value::Timestamp(Timestamp::parse(
                &self.numeric_text(wire)?,
                *precision,
            )?)),
            Type::Duration => Ok(Value::Duration(Duration::parse(self.expect_string(wire)?)?)),
            Type::Period => self.decode_period(wire),
            Type::Json => Ok(Value::Json(Json::from_wire(wire)?)),
            Type::Blob => self.decode_blob(wire),
            Type::Enum(declared) => {
                Ok(Value::Enum(declared.parse(self.expect_string(wire)?)?))
            }
            Type::Optional(inner) => Self::decode_optional(inner, wire),
            Type::Set(inner) => Self::decode_set(inner, wire),
            Type::Map(key, value) => Self::decode_map(key, value, wire),
            Type::Ref(target) => Self::decode_ref(target, wire),
            Type::Struct(fields) => Self::decode_struct(fields, wire),
            Type::Composite(components) => {
                Ok(Value::Composite(Self::decode_composite(components, wire)?))
            }
            Type::View(_) => Err(ValueError::TypeMismatch {
                ty: "view",
                expected: JsonShape::Null,
                found: JsonShape::of(wire),
            }),
        }
    }

    fn mismatch(&self, expected: JsonShape, wire: &J) -> ValueError {
        ValueError::TypeMismatch {
            ty: self.name(),
            expected,
            found: JsonShape::of(wire),
        }
    }

    fn expect_string<'a>(&self, wire: &'a J) -> Result<&'a str, ValueError> {
        wire.as_str()
            .ok_or_else(|| self.mismatch(JsonShape::String, wire))
    }

    fn expect_bool(&self, wire: &J) -> Result<bool, ValueError> {
        wire.as_bool()
            .ok_or_else(|| self.mismatch(JsonShape::Bool, wire))
    }

    fn expect_object<'a>(&self, wire: &'a J) -> Result<&'a serde_json::Map<String, J>, ValueError> {
        wire.as_object()
            .ok_or_else(|| self.mismatch(JsonShape::Object, wire))
    }

    fn expect_array<'a>(&self, wire: &'a J) -> Result<&'a Vec<J>, ValueError> {
        wire.as_array()
            .ok_or_else(|| self.mismatch(JsonShape::Array, wire))
    }

    /// The canonical form of `int`/`decimal`/`timestamp` is a JSON string; a
    /// bare JSON number is accepted and normalized (SPEC-ISSUES item 2,
    /// least-surprising decoder stance).
    fn numeric_text(&self, wire: &J) -> Result<String, ValueError> {
        match wire {
            J::String(text) => Ok(text.clone()),
            J::Number(number) => Ok(number.to_string()),
            other => Err(self.mismatch(JsonShape::String, other)),
        }
    }

    fn decode_bytes(&self, wire: &J) -> Result<Value, ValueError> {
        let object = self.expect_object(wire)?;
        let payload = object
            .get("$bytes")
            .ok_or_else(|| ValueError::MissingMember("$bytes".to_owned()))?;
        let text = payload
            .as_str()
            .ok_or_else(|| self.mismatch(JsonShape::String, payload))?;
        Ok(Value::Bytes(Bytes::from_base64(text)?))
    }

    fn decode_blob(&self, wire: &J) -> Result<Value, ValueError> {
        let object = self.expect_object(wire)?;
        let sha = Self::required_str(object, "$sha512")?;
        let bytes_text = Self::required_str(object, "$bytes")?;
        let media = Self::required_str(object, "$media")?;
        let name = match object.get("$name") {
            Some(J::String(text)) => Some(text.clone()),
            Some(_) => return Err(ValueError::UnexpectedMember("$name".to_owned())),
            None => None,
        };
        let byte_count: u64 = bytes_text
            .parse()
            .map_err(|_| ValueError::MalformedInt(bytes_text.to_owned()))?;
        let descriptor = BlobDescriptor::new(
            Sha512::parse(sha)?,
            byte_count,
            MediaType::new(media.to_owned()),
            name,
        );
        Ok(Value::Blob(Box::new(descriptor)))
    }

    fn required_str<'a>(
        object: &'a serde_json::Map<String, J>,
        key: &str,
    ) -> Result<&'a str, ValueError> {
        object
            .get(key)
            .ok_or_else(|| ValueError::MissingMember(key.to_owned()))?
            .as_str()
            .ok_or_else(|| ValueError::TypeMismatch {
                ty: "blob",
                expected: JsonShape::String,
                found: JsonShape::of(object.get(key).unwrap_or(&J::Null)),
            })
    }

    fn decode_period(&self, wire: &J) -> Result<Value, ValueError> {
        match wire {
            J::String(text) => {
                // A.4: fixed period, day/time only; calendar components rejected.
                Ok(Value::Period(Box::new(Period::Fixed(Duration::parse(text)?))))
            }
            J::Object(members) => {
                let calendar = Self::build_calendar(members)?;
                Ok(Value::Period(Box::new(Period::Calendar(calendar))))
            }
            other => Err(self.mismatch(JsonShape::Object, other)),
        }
    }

    fn build_calendar(
        members: &serde_json::Map<String, J>,
    ) -> Result<crate::period::CalendarPeriod, ValueError> {
        let mut builder = CalendarPeriodBuilder::default();
        for (key, value) in members {
            match key.as_str() {
                "years" => builder.years = Self::magnitude(value, "years")?,
                "months" => builder.months = Self::magnitude(value, "months")?,
                "weeks" => builder.weeks = Self::magnitude(value, "weeks")?,
                "days" => builder.days = Self::magnitude(value, "days")?,
                "time" => {
                    let text = value
                        .as_str()
                        .ok_or_else(|| ValueError::MalformedDuration {
                            text: value.to_string(),
                            reason: "calendar period `time` must be an ISO-8601 string",
                        })?;
                    builder.time = Duration::parse(text)?;
                }
                "zone" => {
                    let text = value
                        .as_str()
                        .ok_or_else(|| ValueError::UnexpectedMember("zone".to_owned()))?;
                    builder.zone = Some(text.to_owned());
                }
                "overflow" => builder.set_overflow(Self::policy_text(value, "overflow")?)?,
                "ambiguous" => builder.set_ambiguous(Self::policy_text(value, "ambiguous")?)?,
                "missing" => builder.set_missing(Self::policy_text(value, "missing")?)?,
                other => return Err(ValueError::UnexpectedMember(other.to_owned())),
            }
        }
        builder.build()
    }

    fn magnitude(value: &J, field: &'static str) -> Result<i64, ValueError> {
        value.as_i64().ok_or(ValueError::UnknownPolicy {
            field,
            value: value.to_string(),
        })
    }

    fn policy_text<'a>(value: &'a J, field: &'static str) -> Result<&'a str, ValueError> {
        value.as_str().ok_or(ValueError::UnknownPolicy {
            field,
            value: value.to_string(),
        })
    }

    fn decode_optional(inner: &Type, wire: &J) -> Result<Value, ValueError> {
        if let J::Object(members) = wire
            && members.get("$none") == Some(&J::Bool(true))
        {
            return Ok(Value::None);
        }
        inner.decode(wire)
    }

    fn decode_set(inner: &Type, wire: &J) -> Result<Value, ValueError> {
        let items = Type::Set(Box::new(inner.clone())).expect_array(wire)?;
        let mut members = BTreeSet::new();
        for item in items {
            members.insert(inner.decode(item)?);
        }
        Ok(Value::Set(members))
    }

    fn decode_map(key: &Type, value: &Type, wire: &J) -> Result<Value, ValueError> {
        let entries = Type::Map(Box::new(key.clone()), Box::new(value.clone()))
            .expect_array(wire)?;
        let mut map = BTreeMap::new();
        for entry in entries {
            let pair = entry.as_array().ok_or(ValueError::TypeMismatch {
                ty: "map",
                expected: JsonShape::Array,
                found: JsonShape::of(entry),
            })?;
            let (Some(k), Some(v), 2) = (pair.first(), pair.get(1), pair.len()) else {
                return Err(ValueError::CompositeArity {
                    expected: 2,
                    found: pair.len(),
                });
            };
            map.insert(key.decode(k)?, value.decode(v)?);
        }
        Ok(Value::Map(map))
    }

    fn decode_ref(target: &RefTarget, wire: &J) -> Result<Value, ValueError> {
        match target {
            RefTarget::Scalar(inner) => Ok(Value::Ref(Ref::scalar(inner.decode(wire)?))),
            RefTarget::Composite(components) => {
                Ok(Value::Ref(Ref::composite(Self::decode_composite(components, wire)?)))
            }
        }
    }

    /// Decode a composite key's component values in `$key` order (A.9). The wire
    /// value is either the canonical `$key`-order array of component wire values,
    /// or the authoring object `{ name: … }` naming each component (normalized to
    /// `$key` order here — object member order carries no meaning). Either form
    /// yields the components in the declared `$key` order.
    fn decode_composite(
        components: &[(String, Type)],
        wire: &J,
    ) -> Result<Vec<Value>, ValueError> {
        match wire {
            J::Array(array) => {
                if array.len() != components.len() {
                    return Err(ValueError::CompositeArity {
                        expected: components.len(),
                        found: array.len(),
                    });
                }
                components
                    .iter()
                    .zip(array)
                    .map(|((_, ty), item)| ty.decode(item))
                    .collect()
            }
            J::Object(object) => {
                for member in object.keys() {
                    if !components.iter().any(|(name, _)| name == member) {
                        return Err(ValueError::UnexpectedMember(member.clone()));
                    }
                }
                components
                    .iter()
                    .map(|(name, ty)| match object.get(name) {
                        Some(item) => ty.decode(item),
                        None => Err(ValueError::MissingMember(name.clone())),
                    })
                    .collect()
            }
            other => Err(ValueError::TypeMismatch {
                ty: "composite key",
                expected: JsonShape::Array,
                found: JsonShape::of(other),
            }),
        }
    }

    fn decode_struct(fields: &StructType, wire: &J) -> Result<Value, ValueError> {
        let object = Type::Struct(fields.clone()).expect_object(wire)?;
        for member in object.keys() {
            if fields.field(member).is_none() {
                return Err(ValueError::UnexpectedMember(member.clone()));
            }
        }
        let mut decoded = Vec::new();
        for (name, field_type) in fields.fields() {
            match object.get(name) {
                Some(member) => {
                    decoded.push((Text::new(name.clone()), field_type.decode(member)?));
                }
                None => {
                    // A.1: an absent optional field is `none`; a required field is an error.
                    if matches!(field_type, Type::Optional(_)) {
                        decoded.push((Text::new(name.clone()), Value::None));
                    } else {
                        return Err(ValueError::MissingMember(name.clone()));
                    }
                }
            }
        }
        Ok(Value::Struct(Struct::new(decoded)))
    }
}
