//! Wire codec for the durable records: [`RowAddress`], [`CommittedRowOp`], the
//! `commit_log` row, and the `instance_meta` composition/position columns.
//!
//! Addresses and log operations are built from typed [`Value`]s, so they inherit
//! the same schema-free constraint as [`crate::value_codec`]: they must persist
//! in a self-describing form the store can decode without a schema. Both encode
//! to pure JSON arrays of tagged values — the form the durable `commit_log` carries
//! each op (and its addresses) in, decodable on load without a schema.
//!
//! Alongside the per-op codec, this module owns the record decoders the store's
//! SQL read paths share: [`decode_log_row`] (a whole `commit_log` row → a
//! [`CommittedTransition`], folded by `snapshot` §4.3 and returned by `log_from`
//! §4.4), [`seq_from`] (a stored `BIGINT` position → [`CommitSeq`]), and
//! [`encode_composition`]/[`decode_composition`] (the `instance_meta.composition`
//! JSONB). They moved here from the deleted in-memory projection: they are wire
//! codecs, not a read model (`DESIGN-pure-pg.md` §4.3, Phase 3).

use liasse_ident::{
    DefinitionId, HistoryPoint, InstanceId, LineageId, NameSegment, PointId, RowIncarnation,
    TransactionId,
};
use liasse_store::{
    AddressStep, CommitSeq, CommittedRowOp, CommittedTransition, Composition, Mount, PackagePin,
    RowAddress, StoreError, key_from_components,
};
use liasse_value::{Sha512, Value};
use postgres::Row;
use serde_json::{Map, Value as J};

use crate::backend::cell;
use crate::{jsonb_text, value_codec};

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

/// Decode one `commit_log` row (`seq`, `transaction_id`, `ops`, `created`) into a
/// [`CommittedTransition`]. Shared by `snapshot`'s frontier log fold (§4.3) and the
/// leaf `log_from` read (§4.4), so both decode a stored transition identically. The
/// `created` instant (§22.5) is what a fold uses to reconstruct each inserted row's
/// `$created` (§14.1/§22.6).
pub(crate) fn decode_log_row(row: &Row) -> Result<CommittedTransition, StoreError> {
    let seq = seq_from(cell::<i64>(row, "commit_log", "seq")?, "commit_log.seq")?;
    let transaction = cell::<Option<String>>(row, "commit_log", "transaction_id")?
        .map(|id| TransactionId::new(jsonb_text::decode_text(&id)));
    let ops_wire = jsonb_text::from_jsonb(&cell::<J>(row, "commit_log", "ops")?);
    let ops = ops_wire
        .as_array()
        .ok_or_else(|| corrupt("commit_log ops is not an array"))?
        .iter()
        .map(decode_op)
        .collect::<Result<Vec<_>, _>>()?;
    let created =
        value_codec::decode_created(&jsonb_text::from_jsonb(&cell::<J>(row, "commit_log", "created")?))?;
    Ok(CommittedTransition::new(seq, ops, created, transaction))
}

/// Encode a composition into the `instance_meta.composition` JSONB.
pub(crate) fn encode_composition(composition: &Composition) -> J {
    let mut obj = Map::new();
    for (name, mount) in composition.mounts() {
        let mut entry = Map::new();
        entry.insert("instance".to_owned(), J::String(mount.instance().as_str().to_owned()));
        entry.insert("lineage".to_owned(), J::String(mount.selected().lineage().as_str().to_owned()));
        entry.insert("point".to_owned(), J::String(mount.selected().point().as_str().to_owned()));
        // §5.1/§13.10: the decoded-package provenance pinned on a lifecycle-mounted
        // instance, when present.
        if let Some(pin) = mount.package() {
            let mut pkg = Map::new();
            pkg.insert("content".to_owned(), J::String(pin.content().to_canonical_text()));
            pkg.insert("definition".to_owned(), J::String(pin.definition().to_canonical_text()));
            let [major, minor, patch] = pin.version();
            pkg.insert("version".to_owned(), J::Array(vec![J::from(major), J::from(minor), J::from(patch)]));
            entry.insert("package".to_owned(), J::Object(pkg));
        }
        obj.insert(name.to_owned(), J::Object(entry));
    }
    J::Object(obj)
}

/// Decode a composition from the `instance_meta.composition` JSONB — the inverse of
/// [`encode_composition`], serving the leaf `composition` read (§4.4).
pub(crate) fn decode_composition(wire: &J) -> Result<Composition, StoreError> {
    let obj = wire.as_object().ok_or_else(|| corrupt("composition is not an object"))?;
    let mut composition = Composition::new();
    for (name, entry) in obj {
        let entry = entry.as_object().ok_or_else(|| corrupt("mount is not an object"))?;
        let field = |key: &str| {
            entry.get(key).and_then(J::as_str).ok_or_else(|| corrupt(format!("mount missing `{key}`")))
        };
        let instance = InstanceId::new(field("instance")?);
        let point = HistoryPoint::new(LineageId::new(field("lineage")?), PointId::new(field("point")?));
        let mount = match entry.get("package") {
            Some(package) => Mount::pinned(instance, point, decode_package_pin(package)?),
            None => Mount::new(instance, point),
        };
        composition = composition.with(name.clone(), mount);
    }
    Ok(composition)
}

/// Decode a mount's `package` provenance sub-object into a [`PackagePin`] (§5.1) —
/// the inverse of the encode above.
fn decode_package_pin(wire: &J) -> Result<PackagePin, StoreError> {
    let obj = wire.as_object().ok_or_else(|| corrupt("mount `package` is not an object"))?;
    let text = |key: &str| obj.get(key).and_then(J::as_str).ok_or_else(|| corrupt(format!("`package` missing `{key}`")));
    let content = Sha512::parse(text("content")?).map_err(|error| corrupt(format!("bad `package.content`: {error}")))?;
    let definition =
        DefinitionId::parse(text("definition")?).map_err(|error| corrupt(format!("bad `package.definition`: {error}")))?;
    let version = obj.get("version").and_then(J::as_array).ok_or_else(|| corrupt("`package` missing `version`"))?;
    let mut parts = [0u64; 3];
    if version.len() != 3 {
        return Err(corrupt("`package.version` is not a three-part version"));
    }
    for (slot, value) in parts.iter_mut().zip(version) {
        *slot = value.as_u64().ok_or_else(|| corrupt("`package.version` component is not a u64"))?;
    }
    Ok(PackagePin::new(content, definition, parts))
}

/// Rebuild the serial position stored as the durable `BIGINT` `raw` (from column
/// `what`). A position is minted by [`CommitSeq::next`] and can never be
/// negative; a negative durable value is a corruption to report, never a value
/// to silently coerce to genesis. Reconstruction is O(1) via
/// [`CommitSeq::from_stored`]. Shared by the reads that decode a stored position
/// (`head`, `point_position`, `commit_transition`) and by the log decode.
pub(crate) fn seq_from(raw: i64, what: &str) -> Result<CommitSeq, StoreError> {
    let n = u64::try_from(raw).map_err(|_| corrupt(format!("{what} is negative ({raw})")))?;
    Ok(CommitSeq::from_stored(n))
}
