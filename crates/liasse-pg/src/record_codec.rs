//! Wire codec for the structural records: [`RowAddress`] and [`CommittedRowOp`].
//!
//! Addresses and log operations are built from typed [`Value`]s, so they inherit
//! the same schema-free constraint as [`crate::value_codec`]: they must persist
//! in a self-describing form the store can decode without a schema. Both encode
//! to pure JSON arrays of tagged values, which makes the compact string form of
//! an address deterministic — [`address_key`] uses it as the durable primary key
//! of the `rows` table, injective because the value codec is.

use liasse_ident::{NameSegment, RowIncarnation};
use liasse_store::{AddressStep, CommittedRowOp, RowAddress, StoreError, key_from_components};
use liasse_value::Value;
use serde_json::{Map, Value as J};

use crate::value_codec;

/// The deterministic compact-JSON string of an address — its `rows` primary key.
///
/// A serialization failure must never be swallowed into the empty string: that
/// would make two distinct addresses share the primary key `""`, breaking the
/// injectivity the `rows` table relies on. The error is propagated as a
/// corruption instead.
pub fn address_key(address: &RowAddress) -> Result<String, StoreError> {
    serde_json::to_string(&encode_address(address))
        .map_err(|error| corrupt(format!("address key could not be serialized: {error}")))
}

/// Encode a row address as a JSON array of `[name, [key-components…]]` steps.
///
/// Key components go through [`value_codec::encode_key`], not the plain value
/// encoder: an address key is a durable *identity*, so it must collapse
/// Annex-B-equal keys (scale-variant decimals, precision-variant timestamps) to
/// one string — otherwise a committed update/delete addressed by a numerically
/// equal key would target a different row than its insert.
#[must_use]
pub fn encode_address(address: &RowAddress) -> J {
    J::Array(
        address
            .steps()
            .map(|step| {
                J::Array(vec![
                    J::String(step.name().as_str().to_owned()),
                    J::Array(step.key().components().map(value_codec::encode_key).collect()),
                ])
            })
            .collect(),
    )
}

/// Decode an address produced by [`encode_address`].
pub fn decode_address(wire: &J) -> Result<RowAddress, StoreError> {
    let steps = wire.as_array().ok_or_else(|| corrupt("address is not an array"))?;
    let mut decoded = steps.iter().map(decode_step);
    let first = decoded
        .next()
        .ok_or_else(|| corrupt("address has no steps"))??;
    let mut address = RowAddress::root(first);
    for step in decoded {
        address = address.child(step?);
    }
    Ok(address)
}

fn decode_step(wire: &J) -> Result<AddressStep, StoreError> {
    let pair = wire.as_array().ok_or_else(|| corrupt("address step is not an array"))?;
    let name = pair.first().and_then(J::as_str).ok_or_else(|| corrupt("step missing name"))?;
    let components = pair.get(1).and_then(J::as_array).ok_or_else(|| corrupt("step missing key"))?;
    let values: Vec<Value> = components.iter().map(value_codec::decode).collect::<Result<_, _>>()?;
    let key = key_from_components(values)?;
    Ok(AddressStep::new(NameSegment::new(name), key))
}

/// Encode one committed row operation for the log's `ops` array.
#[must_use]
pub fn encode_op(op: &CommittedRowOp) -> J {
    match op {
        CommittedRowOp::Insert { address, incarnation, value } => {
            tag("insert", vec![encode_address(address), incar(incarnation), value_codec::encode(value)])
        }
        CommittedRowOp::Update { address, incarnation, value } => {
            tag("update", vec![encode_address(address), incar(incarnation), value_codec::encode(value)])
        }
        CommittedRowOp::Delete { address, incarnation } => {
            tag("delete", vec![encode_address(address), incar(incarnation)])
        }
        CommittedRowOp::Rekey { from, to, incarnation, value } => tag(
            "rekey",
            vec![encode_address(from), encode_address(to), incar(incarnation), value_codec::encode(value)],
        ),
    }
}

/// Decode one committed row operation produced by [`encode_op`].
pub fn decode_op(wire: &J) -> Result<CommittedRowOp, StoreError> {
    let (kind, body) = single_member(wire)?;
    let parts = body.as_array().ok_or_else(|| corrupt("op body is not an array"))?;
    let at = |index: usize| parts.get(index).ok_or_else(|| corrupt("op body too short"));
    match kind {
        "insert" => Ok(CommittedRowOp::Insert {
            address: decode_address(at(0)?)?,
            incarnation: decode_incar(at(1)?)?,
            value: value_codec::decode(at(2)?)?,
        }),
        "update" => Ok(CommittedRowOp::Update {
            address: decode_address(at(0)?)?,
            incarnation: decode_incar(at(1)?)?,
            value: value_codec::decode(at(2)?)?,
        }),
        "delete" => Ok(CommittedRowOp::Delete {
            address: decode_address(at(0)?)?,
            incarnation: decode_incar(at(1)?)?,
        }),
        "rekey" => Ok(CommittedRowOp::Rekey {
            from: decode_address(at(0)?)?,
            to: decode_address(at(1)?)?,
            incarnation: decode_incar(at(2)?)?,
            value: value_codec::decode(at(3)?)?,
        }),
        other => Err(corrupt(format!("unknown op kind `{other}`"))),
    }
}

fn incar(incarnation: &RowIncarnation) -> J {
    J::String(incarnation.as_str().to_owned())
}

fn decode_incar(wire: &J) -> Result<RowIncarnation, StoreError> {
    wire.as_str()
        .map(RowIncarnation::new)
        .ok_or_else(|| corrupt("incarnation is not a string"))
}

fn tag(name: &str, parts: Vec<J>) -> J {
    let mut obj = Map::new();
    obj.insert(name.to_owned(), J::Array(parts));
    J::Object(obj)
}

fn single_member(wire: &J) -> Result<(&str, &J), StoreError> {
    let obj = wire.as_object().ok_or_else(|| corrupt("op is not an object"))?;
    let mut iter = obj.iter();
    match (iter.next(), iter.next()) {
        (Some((kind, body)), None) => Ok((kind.as_str(), body)),
        _ => Err(corrupt("an op must be a single-member object")),
    }
}

fn corrupt(detail: impl Into<String>) -> StoreError {
    StoreError::Corruption { detail: detail.into() }
}
