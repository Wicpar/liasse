//! Genesis seed admission (§9.1): `$data` rows pass through the same defaults,
//! normalization, checks, and key/ref/uniqueness rules as mutation inserts.
//!
//! CORE scope covers top-level keyed collections whose rows carry scalar, ref,
//! and set fields, keyed by a single `$key` field. Nested keyed collections in a
//! seed row and composite seed keys are documented seams.

use liasse_syntax::DocValue;
use liasse_store::RowAddress;
use liasse_value::Type;

use crate::compiled::{Compiled, CompiledCollection};
use crate::doc;
use crate::error::{Rejection, RejectionReason};
use crate::eval::EvalCtx;
use crate::materialize::{self, FieldMap};
use crate::rules;
use crate::state::Prospective;

/// Admit every `$data` row into the prospective state, recording touched
/// addresses for the final rule pass.
pub(crate) fn admit(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &mut Prospective,
    touched: &mut Vec<RowAddress>,
    data: &DocValue,
) -> Result<(), Rejection> {
    let Some(collections) = doc::object(data) else {
        return Err(Rejection::new(RejectionReason::Malformed, "`$data` must be an object"));
    };
    for member in collections {
        let Some(collection) = compiled.collection(&member.name.text) else {
            // §9.1: only writable state can be seeded; unknown/computed members
            // are not seedable. A non-collection seed key is a CORE seam.
            continue;
        };
        admit_collection(ctx, prospective, touched, collection, &member.value)?;
    }
    Ok(())
}

fn admit_collection(
    ctx: &EvalCtx<'_>,
    prospective: &mut Prospective,
    touched: &mut Vec<RowAddress>,
    collection: &CompiledCollection,
    rows: &DocValue,
) -> Result<(), Rejection> {
    let Some(entries) = doc::object(rows) else {
        return Err(Rejection::new(
            RejectionReason::Malformed,
            format!("`$data.{}` must map keys to rows", collection.name),
        ));
    };
    for entry in entries {
        let mut fields = decode_row(collection, &entry.name.text, &entry.value)?;
        rules::apply_defaults(collection, &mut fields, ctx, prospective)?;
        rules::normalize_all(collection, &mut fields, ctx, prospective)?;
        let address = key_address(ctx, collection, &fields)?;
        if prospective.contains(&address) {
            return Err(Rejection::new(RejectionReason::DuplicateKey, "duplicate seed key")
                .at(address.render()));
        }
        prospective.insert(address.clone(), fields);
        touched.push(address);
    }
    Ok(())
}

/// Decode a seed row's declared fields against their types, supplying the local
/// key from the map member when the row omits it (§9.1).
fn decode_row(
    collection: &CompiledCollection,
    key_text: &str,
    row: &DocValue,
) -> Result<FieldMap, Rejection> {
    let members = doc::object(row)
        .ok_or_else(|| Rejection::new(RejectionReason::Malformed, "a seed row must be an object"))?;
    let mut fields = FieldMap::new();
    for field in &collection.fields {
        let supplied = members.iter().find(|m| m.name.text == field.name);
        let value = match supplied {
            Some(member) => decode(&field.ty, &doc::to_json(&member.value), &field.name)?,
            None if collection.key.first().is_some_and(|k| k == &field.name) => {
                decode(&field.ty, &serde_json::Value::String(key_text.to_owned()), &field.name)?
            }
            None => continue,
        };
        fields.insert(field.name.clone(), value);
    }
    Ok(fields)
}

fn decode(ty: &Type, wire: &serde_json::Value, field: &str) -> Result<liasse_value::Value, Rejection> {
    ty.decode(wire).map_err(|error| {
        Rejection::new(RejectionReason::TypeError, format!("seed field `{field}`: {error}"))
    })
}

fn key_address(
    ctx: &EvalCtx<'_>,
    collection: &CompiledCollection,
    fields: &FieldMap,
) -> Result<RowAddress, Rejection> {
    let model = ctx
        .schema
        .top_collection(&collection.name)
        .ok_or_else(|| Rejection::new(RejectionReason::Malformed, "unknown seed collection"))?;
    let key = materialize::row_key(model, fields)
        .ok_or_else(|| Rejection::new(RejectionReason::Malformed, "seed row is missing a key field"))?;
    Ok(materialize::top_address(&collection.name, key))
}
