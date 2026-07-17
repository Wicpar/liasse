//! Order-preserving `BYTEA` encoding of a storage key (Stage 1, encode-only).
//!
//! A `key_enc` byte string is laid out so PostgreSQL's native `bytea` comparison
//! (unsigned `memcmp`, then shorter-first) reproduces [`KeyValue`]'s Annex-B
//! [`Ord`] exactly. That single property — plus its canonicality half — is what
//! the accompanying proptest gates:
//!
//! ```text
//! sign(memcmp(encode a, encode b)) == sign(a.cmp(b))
//! a.cmp(b) == Equal            <=>  encode a == encode b   (byte-identical)
//! ```
//!
//! # Layout
//!
//! A [`KeyValue`] is the **plain concatenation** of its component units, no outer
//! framing. Each [`Value`] unit is `[rank byte] ++ [order-preserving body]` and is
//! *prefix-free*: no unit is a proper byte-prefix of another, so concatenation
//! reproduces lexicographic-with-shorter-first component order — matching the
//! derived `Ord` on `KeyValue`/`RowAddress`. The leading rank byte
//! ([`Value`]'s cross-type rank; `bool`=0 … `map`=16, `none`=0xFF) decides
//! cross-type order at byte 0, and `none`'s `0xFF` sorts it after every present
//! value (B.2).
//!
//! # Prefix-free primitives
//!
//! - **Escaped bytes** (`text`/`bytes`/labels/names/zones): every `0x00` becomes
//!   `0x00 0xFF`, then a `0x00 0x00` terminator. `0x00 0x00` never occurs inside
//!   a body, so it is an unambiguous, prefix-free end marker; byte order is
//!   preserved and the empty value is the minimum.
//! - **Sequence framing** (`struct`/`set`/`map`/`ref`-composite/`json`
//!   array+object): `0x01` before each item, a final `0x00` stop. A shorter
//!   sequence hits `0x00` where a longer one has `0x01`, sorting it first.
//! - **Numbers** live in [`crate::key_enc_num`] (sign-magnitude int / exponent,
//!   offset-binary fixed-width instants).
//!
//! # Canonicality
//!
//! Annex-B-equal values MUST encode byte-identically or a durable lookup by a
//! scale/precision-variant key would miss its row. It is achieved *intrinsically*
//! — a `decimal` normalizes to `(exponent, trailing-zero-stripped digits)`, a
//! `timestamp` reduces to its exact `(seconds, sub-nanos)` instant regardless of
//! declared precision — and, defensively, every value is first mapped through
//! [`value_codec::canonical_key`], the same Annex-B canonicalizer the JSONB
//! `key_wire` column uses.

use liasse_value::bigdecimal::BigDecimal;
use liasse_value::num_bigint::Sign;
use liasse_value::{
    Ambiguous, BlobDescriptor, Date, EnumValue, Json, Missing, Overflow, Period, Ref, RefKey,
    Timestamp, Uuid, Value,
};
use liasse_store::KeyValue;

use crate::key_enc_num;
use crate::value_codec;

/// Marker before each item of a framed sequence.
const SEQ_ITEM: u8 = 0x01;
/// Terminator after the last item of a framed sequence.
const SEQ_STOP: u8 = 0x00;

/// Encode a whole key: the concatenation of its per-component units.
pub(crate) fn encode_key_value(key: &KeyValue) -> Vec<u8> {
    let mut out = Vec::new();
    for component in key.components() {
        encode_value(component, &mut out);
    }
    out
}

/// Append one value's prefix-free unit, first mapping it to its Annex-B
/// canonical representative.
pub(crate) fn encode_value(value: &Value, out: &mut Vec<u8>) {
    put(&value_codec::canonical_key(value), out);
}

/// Append the `[rank byte] ++ [body]` unit of an already-canonical value. Nested
/// values recurse through here (not [`encode_value`]) since the whole tree was
/// canonicalized once at the top.
fn put(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Bool(flag) => {
            out.push(0x00);
            out.push(u8::from(*flag));
        }
        Value::Int(integer) => {
            out.push(0x01);
            key_enc_num::write_int(out, integer.as_bigint());
        }
        Value::Decimal(decimal) => {
            out.push(0x02);
            put_number(decimal.as_big_decimal(), out);
        }
        Value::Text(text) => {
            out.push(0x03);
            put_escaped(text.as_str().as_bytes(), out);
        }
        Value::Bytes(bytes) => {
            out.push(0x04);
            put_escaped(bytes.as_slice(), out);
        }
        Value::Uuid(uuid) => {
            out.push(0x05);
            put_uuid(uuid, out);
        }
        Value::Date(date) => {
            out.push(0x06);
            put_date(*date, out);
        }
        Value::Timestamp(timestamp) => {
            out.push(0x07);
            put_timestamp(*timestamp, out);
        }
        Value::Duration(duration) => {
            out.push(0x08);
            key_enc_num::write_ob_i128(out, duration.as_nanos());
        }
        Value::Period(period) => {
            out.push(0x09);
            put_period(period, out);
        }
        Value::Enum(enumeration) => {
            out.push(0x0A);
            put_enum(enumeration, out);
        }
        Value::Ref(reference) => {
            out.push(0x0B);
            put_ref(reference, out);
        }
        Value::Json(json) => {
            out.push(0x0C);
            put_json(json, out);
        }
        Value::Blob(blob) => {
            out.push(0x0D);
            put_blob(blob, out);
        }
        Value::Struct(fields) => {
            out.push(0x0E);
            for (name, field) in fields.fields() {
                out.push(SEQ_ITEM);
                put_escaped(name.as_str().as_bytes(), out);
                put(field, out);
            }
            out.push(SEQ_STOP);
        }
        Value::Set(members) => {
            out.push(0x0F);
            for member in members {
                out.push(SEQ_ITEM);
                put(member, out);
            }
            out.push(SEQ_STOP);
        }
        Value::Map(entries) => {
            out.push(0x10);
            for (key, val) in entries {
                out.push(SEQ_ITEM);
                put(key, out);
                put(val, out);
            }
            out.push(SEQ_STOP);
        }
        Value::None => out.push(0xFF),
    }
}

/// Escaped variable-length bytes: `0x00 -> 0x00 0xFF`, then a `0x00 0x00`
/// terminator (prefix-free, order-preserving).
fn put_escaped(payload: &[u8], out: &mut Vec<u8>) {
    for &byte in payload {
        out.push(byte);
        if byte == 0x00 {
            out.push(0xFF);
        }
    }
    out.push(0x00);
    out.push(0x00);
}

/// A `decimal`/`json`-number body: sign class, then for a non-zero value the
/// normalized exponent and the trailing-zero-stripped significand digits with a
/// `0x00` terminator; a negative value inverts that whole body so magnitude order
/// reverses. A zero of any scale is the sign class alone (so `0 == 0.00`).
fn put_number(value: &BigDecimal, out: &mut Vec<u8>) {
    let (mantissa, scale) = value.as_bigint_and_exponent();
    let sign = mantissa.sign();
    if sign == Sign::NoSign {
        out.push(key_enc_num::SIGN_ZERO);
        return;
    }
    // value = mantissa * 10^-scale; write it as 0.<digits> * 10^exponent with the
    // significand's trailing zeros stripped, so 1.0, 1.00 and 1 share one body.
    let digits = mantissa.magnitude().to_str_radix(10);
    let exponent = digits.len() as i64 - scale;
    let significand = digits.trim_end_matches('0');
    let mut body = Vec::new();
    key_enc_num::write_signed_i64(&mut body, exponent);
    body.extend_from_slice(significand.as_bytes());
    body.push(0x00);
    if sign == Sign::Minus {
        out.push(key_enc_num::SIGN_NEG);
        out.extend(body.iter().map(|byte| !byte));
    } else {
        out.push(key_enc_num::SIGN_POS);
        out.extend_from_slice(&body);
    }
}

/// The 16 raw bytes of a UUID (their `memcmp` is `Uuid`'s `Ord`), recovered from
/// the canonical hyphenated lowercase-hex text.
fn put_uuid(value: &Uuid, out: &mut Vec<u8>) {
    let text = value.to_canonical_text();
    let hex: String = text.chars().filter(|&nibble| nibble != '-').collect();
    match data_encoding::HEXLOWER.decode(hex.as_bytes()) {
        Ok(bytes) => out.extend_from_slice(&bytes),
        // Unreachable: canonical UUID text is always 32 lowercase hex nibbles.
        Err(_) => out.extend_from_slice(&[0u8; 16]),
    }
}

/// `(year, month, day)` — offset-binary year then raw month/day — lexicographic
/// over which is chronological order.
fn put_date(value: Date, out: &mut Vec<u8>) {
    key_enc_num::write_ob_i32(out, i32::from(value.year()));
    out.push(value.month().unsigned_abs());
    out.push(value.day().unsigned_abs());
}

/// The canonical `(seconds, sub-nanos)` instant, offset-binary seconds then
/// big-endian sub-nanos — precision-independent, so `(1000, ms)` and `(1, s)`
/// coincide.
fn put_timestamp(value: Timestamp, out: &mut Vec<u8>) {
    let ticks = value.precision().ticks_per_second();
    let seconds = value.count().div_euclid(ticks);
    let sub_nanos = value.count().rem_euclid(ticks) * (1_000_000_000 / ticks);
    key_enc_num::write_ob_i128(out, seconds);
    let sub = u32::try_from(sub_nanos).unwrap_or(u32::MAX);
    out.extend_from_slice(&sub.to_be_bytes());
}

/// Big-endian ordinal then escaped label — `EnumValue`'s `(ordinal, label)` order.
fn put_enum(value: &EnumValue, out: &mut Vec<u8>) {
    out.extend_from_slice(&value.ordinal().to_be_bytes());
    put_escaped(value.label().as_bytes(), out);
}

/// A `ref`: scalar (`0x00`) sorts before composite (`0x01`); the inner key(s)
/// recurse.
fn put_ref(value: &Ref, out: &mut Vec<u8>) {
    match value.key() {
        RefKey::Scalar(inner) => {
            out.push(0x00);
            put(inner, out);
        }
        RefKey::Composite(components) => {
            out.push(0x01);
            for component in components {
                out.push(SEQ_ITEM);
                put(component, out);
            }
            out.push(SEQ_STOP);
        }
    }
}

/// A `json` value bodied by its B.3 kind rank (`null`0 … `object`5) then payload.
fn put_json(value: &Json, out: &mut Vec<u8>) {
    match value {
        Json::Null => out.push(0x00),
        Json::Bool(flag) => {
            out.push(0x01);
            out.push(u8::from(*flag));
        }
        Json::Number(number) => {
            out.push(0x02);
            put_number(number, out);
        }
        Json::String(text) => {
            out.push(0x03);
            put_escaped(text.as_bytes(), out);
        }
        Json::Array(items) => {
            out.push(0x04);
            for item in items {
                out.push(SEQ_ITEM);
                put_json(item, out);
            }
            out.push(SEQ_STOP);
        }
        Json::Object(members) => {
            out.push(0x05);
            for (name, member) in members {
                out.push(SEQ_ITEM);
                put_escaped(name.as_bytes(), out);
                put_json(member, out);
            }
            out.push(SEQ_STOP);
        }
    }
}

/// A `period`: fixed (`0x00`) before calendar (`0x01`); calendar lays out the
/// B.1 field tuple `(years, months, weeks, days, time, zone, policies…)`.
fn put_period(value: &Period, out: &mut Vec<u8>) {
    match value {
        Period::Fixed(duration) => {
            out.push(0x00);
            key_enc_num::write_ob_i128(out, duration.as_nanos());
        }
        Period::Calendar(calendar) => {
            out.push(0x01);
            let (years, months, weeks, days) = calendar.calendar_magnitudes();
            key_enc_num::write_ob_i64(out, years);
            key_enc_num::write_ob_i64(out, months);
            key_enc_num::write_ob_i64(out, weeks);
            key_enc_num::write_ob_i64(out, days);
            key_enc_num::write_ob_i128(out, calendar.time().as_nanos());
            match calendar.zone() {
                None => out.push(0x00),
                Some(zone) => {
                    out.push(0x01);
                    put_escaped(zone.as_bytes(), out);
                }
            }
            let (overflow, ambiguous, missing) = calendar.policies();
            out.push(overflow_rank(overflow));
            out.push(ambiguous_rank(ambiguous));
            out.push(missing_rank(missing));
        }
    }
}

/// A `blob` descriptor: `(sha512, byte-count, media, name)` — the B.4 order.
fn put_blob(value: &BlobDescriptor, out: &mut Vec<u8>) {
    let text = value.sha512().to_canonical_text();
    match data_encoding::HEXLOWER.decode(text.as_bytes()) {
        Ok(bytes) => out.extend_from_slice(&bytes),
        // Unreachable: canonical SHA-512 text is always 128 lowercase hex nibbles.
        Err(_) => out.extend_from_slice(&[0u8; 64]),
    }
    out.extend_from_slice(&value.byte_count().to_be_bytes());
    put_escaped(value.media().as_str().as_bytes(), out);
    match value.name() {
        None => out.push(0x00),
        Some(name) => {
            out.push(0x01);
            put_escaped(name.as_bytes(), out);
        }
    }
}

/// Declaration-order rank of an `overflow` policy (`clamp < reject`).
fn overflow_rank(value: Overflow) -> u8 {
    match value {
        Overflow::Clamp => 0,
        Overflow::Reject => 1,
    }
}

/// Declaration-order rank of an `ambiguous` policy (`earlier < later < reject`).
fn ambiguous_rank(value: Ambiguous) -> u8 {
    match value {
        Ambiguous::Earlier => 0,
        Ambiguous::Later => 1,
        Ambiguous::Reject => 2,
    }
}

/// Declaration-order rank of a `missing` policy (`forward < backward < reject`).
fn missing_rank(value: Missing) -> u8 {
    match value {
        Missing::Forward => 0,
        Missing::Backward => 1,
        Missing::Reject => 2,
    }
}
