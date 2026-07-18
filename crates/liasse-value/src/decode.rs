//! Decoding strict-JSON wire values against a [`Type`] into canonical
//! [`Value`]s (Annex A). A successful decode is proof of well-formedness.
//!
//! # Two boundaries, two strictnesses (SPEC-ISSUES item 2)
//!
//! Annex A.1 / D.2 pin a single *canonical* wire spelling for every scalar. The
//! two callers of this codec disagree about what to do with a non-canonical
//! spelling, so this module exposes two entry points:
//!
//! - [`Type::decode`] — the **human-authoring boundary** (Annex C `$data`,
//!   authored `$default`s) and the internal round-trip codec (already-canonical
//!   captured/migrated values). A scalar MAY be non-canonical and is canonicalized
//!   on decode, so authoring stays lenient and the build produces the canonical
//!   form. This is the historical, lenient entry; every existing caller keeps it.
//! - [`Type::decode_wire`] — the **machine wire/request boundary**. A scalar MUST
//!   already be canonical; a non-canonical spelling (uppercase uuid, leading-zero
//!   or `+`-signed or `-0` int, non-canonical base64 padding/variant, a duration
//!   or timestamp spelled non-canonically) is rejected as malformed at admission,
//!   never normalized and never accepted as a distinct value.
//!
//! Both share this one recursive codec; only the per-scalar canonicality gate
//! differs, driven by [`DecodeMode`]. The request/response layer that faces an
//! untrusted peer decodes each inbound argument through [`Type::decode_wire`];
//! authored definitions and internal captures decode through [`Type::decode`].

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

/// Which decode boundary is asking (SPEC-ISSUES item 2). See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeMode {
    /// Machine wire/request boundary: scalars MUST be canonical (Annex A.1/D.2).
    /// A non-canonical spelling is rejected, never normalized.
    Wire,
    /// Human-authoring boundary (Annex C `$data`/`$default`): a non-canonical
    /// scalar is accepted and canonicalized on decode.
    Authored,
}

impl Type {
    /// Decode a strict-JSON wire value against this type at the **human-authoring
    /// boundary** (Annex C `$data`/`$default`) and for internal round-trips of
    /// already-canonical values: a non-canonical scalar spelling is accepted and
    /// canonicalized rather than rejected (SPEC-ISSUES item 2). This is the
    /// lenient entry every existing caller uses.
    pub fn decode(&self, wire: &J) -> Result<Value, ValueError> {
        self.decode_as(wire, DecodeMode::Authored)
    }

    /// Decode a strict-JSON wire value against this type at the **machine
    /// wire/request boundary**: scalars must already be canonical (Annex A.1/D.2),
    /// and a non-canonical spelling is rejected as malformed at admission rather
    /// than normalized (SPEC-ISSUES item 2). The request/response layer decodes
    /// each inbound argument from an untrusted peer through this entry.
    pub fn decode_wire(&self, wire: &J) -> Result<Value, ValueError> {
        self.decode_as(wire, DecodeMode::Wire)
    }

    /// The shared recursive codec; `mode` selects the per-scalar canonicality gate.
    fn decode_as(&self, wire: &J, mode: DecodeMode) -> Result<Value, ValueError> {
        match self {
            Type::Text => Ok(Value::Text(Text::new(self.expect_string(wire)?.to_owned()))),
            Type::Bool => Ok(Value::Bool(self.expect_bool(wire)?)),
            Type::Int => {
                let value = Integer::parse(&self.numeric_text(wire)?)?;
                Self::ensure_canonical(mode, wire, "int", value.to_canonical_text())?;
                Ok(Value::Int(value))
            }
            Type::Decimal => {
                let value = Decimal::parse(&self.numeric_text(wire)?)?;
                Self::ensure_canonical(mode, wire, "decimal", value.to_canonical_text())?;
                Ok(Value::Decimal(value))
            }
            Type::Bytes => self.decode_bytes(wire),
            Type::Uuid => {
                let value = Uuid::parse(self.expect_string(wire)?)?;
                Self::ensure_canonical(mode, wire, "uuid", value.to_canonical_text())?;
                Ok(Value::Uuid(value))
            }
            Type::Date => {
                let value = Date::parse(self.expect_string(wire)?)?;
                Self::ensure_canonical(mode, wire, "date", value.to_canonical_text())?;
                Ok(Value::Date(value))
            }
            Type::Timestamp(precision) => {
                let value = Timestamp::parse(&self.numeric_text(wire)?, *precision)?;
                Self::ensure_canonical(mode, wire, "timestamp", value.to_canonical_text())?;
                Ok(Value::Timestamp(value))
            }
            Type::Duration => {
                let value = Duration::parse(self.expect_string(wire)?)?;
                Self::ensure_canonical(mode, wire, "duration", value.to_canonical_text())?;
                Ok(Value::Duration(value))
            }
            Type::Period => self.decode_period(wire, mode),
            Type::Json => Ok(Value::Json(Json::from_wire(wire)?)),
            Type::Blob => self.decode_blob(wire, mode),
            Type::Enum(declared) => Ok(Value::Enum(declared.parse(self.expect_string(wire)?)?)),
            // A.1 / A.7 / SPEC-ISSUES item 29: `none` is absence, not a value with a
            // wire sentinel. A wire `null` disambiguates by the inner type, which is
            // the sole type whose value space could collide with it:
            //
            // - `optional<json>` — JSON `null` is a *present* `json` value (A.7:
            //   "a Liasse optional JSON value has type `json?`, allowing both `null`
            //   and `none`"). Here `none` is expressed only by absence (an omitted
            //   struct member), never by a wire value, so `null` decodes to the
            //   present JSON `null`.
            // - every other `optional<T>` — the inner type has no `null` value, so a
            //   wire `null` is unambiguously `none`. This is also what a `= none`
            //   value expression round-trips to (its wire form is `null`).
            //
            // A present value (non-`null`, or any value under `optional<json>`)
            // decodes against the inner type.
            Type::Optional(inner) => {
                if wire.is_null() && !matches!(**inner, Type::Json) {
                    Ok(Value::None)
                } else {
                    inner.decode_as(wire, mode)
                }
            }
            Type::Set(inner) => Self::decode_set(inner, wire, mode),
            Type::Map(key, value) => Self::decode_map(key, value, wire, mode),
            Type::Ref(target) => Self::decode_ref(target, wire, mode),
            Type::Struct(fields) => Self::decode_struct(fields, wire, mode),
            Type::Composite(components) => {
                Ok(Value::Composite(Self::decode_composite(components, wire, mode)?))
            }
            Type::View(_) => Err(ValueError::TypeMismatch {
                ty: "view",
                expected: JsonShape::Null,
                found: JsonShape::of(wire),
            }),
        }
    }

    /// Reject a non-canonical scalar spelling at the wire boundary (SPEC-ISSUES
    /// item 2). Only a JSON *string* carries a spelling that can drift from
    /// canonical; a bare JSON number (accepted for `int`/`decimal`/`timestamp`)
    /// has one value and is normalized in either mode. Authoring never rejects.
    fn ensure_canonical(
        mode: DecodeMode,
        wire: &J,
        ty: &'static str,
        canonical: String,
    ) -> Result<(), ValueError> {
        match wire {
            J::String(text) => Self::ensure_canonical_text(mode, ty, text, canonical),
            _ => Ok(()),
        }
    }

    /// The string-spelling half of [`Self::ensure_canonical`], reusable where the
    /// canonical member string is already in hand (the blob descriptor's `$sha512`
    /// and `$bytes`, SPEC-ISSUES item 20): at the wire boundary a spelling that
    /// differs from its canonical form is rejected; authoring never rejects.
    fn ensure_canonical_text(
        mode: DecodeMode,
        ty: &'static str,
        found: &str,
        canonical: String,
    ) -> Result<(), ValueError> {
        if mode == DecodeMode::Wire && found != canonical {
            return Err(ValueError::NonCanonicalScalar {
                ty,
                found: found.to_owned(),
                canonical,
            });
        }
        Ok(())
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

    /// The canonical form of `int`/`decimal`/`timestamp` is a JSON string; a bare
    /// JSON number is also accepted (and normalized to the canonical string). At
    /// the wire boundary a *string* spelling is additionally canonicality-checked
    /// by [`Self::ensure_canonical`]; a number carries a single value and needs no
    /// such check.
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
        // A.1: the canonical `bytes` payload is padded standard base64.
        // [`Bytes::from_base64`] decodes only that canonical spelling — a
        // non-canonical padding/variant is rejected as malformed at *both*
        // boundaries (SPEC-ISSUES item 2), so no separate canonicality gate is
        // needed here.
        Ok(Value::Bytes(Bytes::from_base64(text)?))
    }

    fn decode_blob(&self, wire: &J, mode: DecodeMode) -> Result<Value, ValueError> {
        let object = self.expect_object(wire)?;
        let sha = Self::required_str(object, "$sha512")?;
        let bytes_text = Self::required_str(object, "$bytes")?;
        let media = Self::required_str(object, "$media")?;
        let name = match object.get("$name") {
            Some(J::String(text)) => Some(text.clone()),
            Some(_) => return Err(ValueError::UnexpectedMember("$name".to_owned())),
            None => None,
        };
        // Blobs §18.1 / SPEC-ISSUES item 20: the descriptor is a composite value
        // whose members carry their canonical Annex-A wire form. `$sha512` is
        // exactly 128 lowercase-hex characters and `$bytes` is a canonical `int`;
        // at the wire boundary a non-canonical spelling (uppercase hex, a
        // leading-zero count) is rejected as malformed even when it decodes to the
        // correct value — the same canonical-input rule as every other scalar
        // (item 2), matching the §18.7 upload verifier. `Sha512::parse` itself
        // stays the lenient authoring parse, so authored `$data` is canonicalized.
        let digest = Sha512::parse(sha)?;
        Self::ensure_canonical_text(mode, "sha512", sha, digest.to_canonical_text())?;
        let byte_count: u64 = bytes_text
            .parse()
            .map_err(|_| ValueError::MalformedInt(bytes_text.to_owned()))?;
        Self::ensure_canonical_text(mode, "int", bytes_text, byte_count.to_string())?;
        let descriptor =
            BlobDescriptor::new(digest, byte_count, MediaType::new(media.to_owned()), name);
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

    fn decode_period(&self, wire: &J, mode: DecodeMode) -> Result<Value, ValueError> {
        match wire {
            J::String(text) => {
                // A.4: fixed period, day/time only; calendar components rejected.
                let duration = Duration::parse(text)?;
                Self::ensure_canonical(mode, wire, "period", duration.to_canonical_text())?;
                Ok(Value::Period(Box::new(Period::Fixed(duration))))
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

    fn decode_set(inner: &Type, wire: &J, mode: DecodeMode) -> Result<Value, ValueError> {
        let items = Type::Set(Box::new(inner.clone())).expect_array(wire)?;
        let mut members = BTreeSet::new();
        for item in items {
            // A.1 / SPEC-ISSUES item 29: `none` is not a valid set member. A set
            // element type is never `optional<T>` (a set carries present members
            // only), so decoding each element against `inner` never yields `none`;
            // absence is a non-member, expressed by the member simply not appearing.
            members.insert(inner.decode_as(item, mode)?);
        }
        Ok(Value::Set(members))
    }

    fn decode_map(
        key: &Type,
        value: &Type,
        wire: &J,
        mode: DecodeMode,
    ) -> Result<Value, ValueError> {
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
            // A.1 / SPEC-ISSUES item 29: a map never stores a `none` value; absence
            // is the key not being present. Neither the key nor the value type is
            // `optional<T>`, so neither position decodes to `none`.
            map.insert(key.decode_as(k, mode)?, value.decode_as(v, mode)?);
        }
        Ok(Value::Map(map))
    }

    fn decode_ref(target: &RefTarget, wire: &J, mode: DecodeMode) -> Result<Value, ValueError> {
        match target {
            RefTarget::Scalar(inner) => Ok(Value::Ref(Ref::scalar(inner.decode_as(wire, mode)?))),
            RefTarget::Composite(components) => Ok(Value::Ref(Ref::composite(
                Self::decode_composite(components, wire, mode)?,
            ))),
        }
    }

    /// Decode a fixed-arity positional composite's component values in `$key`
    /// order (A.9). The wire value is either the canonical `$key`-order array of
    /// component wire values, or the authoring object `{ name: … }` naming each
    /// component (normalized to `$key` order here — object member order carries no
    /// meaning). Either form yields the components in the declared `$key` order.
    ///
    /// A composite *key*'s components are key-eligible (A.8), which excludes
    /// `optional<T>`, so a composite key never has a `none` slot. A general
    /// positional composite value MAY carry an optional component; because a
    /// position cannot be omitted, its `none` is JSON `null` (SPEC-ISSUES item 29),
    /// decoded back to `none` by [`Self::decode_component`].
    fn decode_composite(
        components: &[(String, Type)],
        wire: &J,
        mode: DecodeMode,
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
                    .map(|((_, ty), item)| Self::decode_component(ty, item, mode))
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
                        Some(item) => Self::decode_component(ty, item, mode),
                        // An absent optional component is `none` (absence); an
                        // absent required component is an error.
                        None if matches!(ty, Type::Optional(_)) => Ok(Value::None),
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

    /// Decode one positional composite component. A position cannot be omitted, so
    /// an optional component's `none` is JSON `null` (SPEC-ISSUES item 29): `null`
    /// is unambiguous here because it is not a canonical wire form for any scalar
    /// type. A present value decodes against the component type. For a positional
    /// `optional<json>` slot `null` is therefore `none`; a *present* JSON `null`
    /// cannot be placed positionally (it must be object/array-wrapped) — the one
    /// residual corner A.1 resolves in favor of `none`.
    fn decode_component(ty: &Type, item: &J, mode: DecodeMode) -> Result<Value, ValueError> {
        if matches!(ty, Type::Optional(_)) && item.is_null() {
            return Ok(Value::None);
        }
        ty.decode_as(item, mode)
    }

    fn decode_struct(
        fields: &StructType,
        wire: &J,
        mode: DecodeMode,
    ) -> Result<Value, ValueError> {
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
                    // A present optional member is a present value: decode it against
                    // the member's declared type. For `optional<json>` a present
                    // `null` is the JSON value `null` (A.7), not `none` — `none` is
                    // absence, and absence is the member being omitted (below).
                    decoded.push((Text::new(name.clone()), field_type.decode_as(member, mode)?));
                }
                None => {
                    // A.1 / item 29: an absent optional field is `none` (absence by
                    // omission); an absent required field is an error.
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
