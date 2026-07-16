//! Genesis seed admission (§9.1): `$data` rows pass through the same defaults,
//! normalization, checks, and key/ref/uniqueness rules as mutation inserts.
//!
//! CORE scope covers keyed collections whose rows carry scalar, ref, and set
//! fields, keyed by a single `$key` field — including nested keyed collections in
//! a seed row (§5.4/§5.5), which a meter's pool/spend arrangement (§15) seeds
//! under an ancestor row. Composite seed keys remain a documented seam.

use liasse_ident::NameSegment;
use liasse_syntax::DocValue;
use liasse_store::{CollectionPath, KeyValue, RowAddress};
use liasse_value::Type;

use crate::compiled::{Compiled, CompiledCollection};
use crate::doc;
use crate::error::{Rejection, RejectionReason};
use crate::eval::EvalCtx;
use crate::materialize::FieldMap;
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
    let model = ctx.schema.model();
    let mut singleton = FieldMap::new();
    for member in collections {
        if let Some(collection) = compiled.collection(&member.name.text) {
            let store_path = CollectionPath::top(NameSegment::new(member.name.text.clone()));
            admit_rows(ctx, prospective, touched, collection, &store_path, &member.value)?;
            continue;
        }
        // §8.2/§9.1: a `$data` member naming a singleton root field seeds that
        // field; an unknown or computed member is not seedable.
        if let Some(node) = model.root().member(&member.name.text).map(|m| &m.node)
            && let Some(ty) = crate::singleton::member_type(model, node)
        {
            let value = decode(&ty, &doc::to_json(&member.value), &member.name.text)?;
            singleton.insert(member.name.text.clone(), value);
        }
    }
    if !singleton.is_empty() {
        prospective.insert(crate::singleton::address(), singleton);
    }
    Ok(())
}

/// Admit every seed row of the collection at `store_path` (top-level or nested,
/// §5.4), then recurse into each row's nested-collection members (§5.5). The
/// address of each row roots the store path of its children so a nested pool or
/// spend keeps its ancestor identity (§15).
fn admit_rows(
    ctx: &EvalCtx<'_>,
    prospective: &mut Prospective,
    touched: &mut Vec<RowAddress>,
    collection: &CompiledCollection,
    store_path: &CollectionPath,
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
        let key = row_key(collection, &fields)?;
        let address = store_path.row(key);
        if prospective.contains(&address) {
            return Err(Rejection::new(RejectionReason::DuplicateKey, "duplicate seed key")
                .at(address.render()));
        }
        prospective.insert(address.clone(), fields);
        touched.push(address.clone());
        // §5.5: a seed row may carry nested-collection initializers, admitted
        // under the parent address through the same pipeline.
        let members = doc::object(&entry.value).into_iter().flatten();
        for member in members {
            if let Some(child) = collection.child(&member.name.text) {
                let child_path = CollectionPath::nested(
                    address.steps().cloned(),
                    NameSegment::new(member.name.text.clone()),
                );
                admit_rows(ctx, prospective, touched, child, &child_path, &member.value)?;
            }
        }
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

/// The [`KeyValue`] of a seed row in `$key` order (§5.4), from the compiled
/// collection's key field list.
fn row_key(collection: &CompiledCollection, fields: &FieldMap) -> Result<KeyValue, Rejection> {
    let mut components = collection.key.iter().map(|field| fields.get(field.as_str()).cloned());
    let first = components
        .next()
        .flatten()
        .ok_or_else(|| Rejection::new(RejectionReason::Malformed, "seed row is missing a key field"))?;
    let mut rest = Vec::new();
    for component in components {
        rest.push(component.ok_or_else(|| {
            Rejection::new(RejectionReason::Malformed, "seed row is missing a key field")
        })?);
    }
    Ok(KeyValue::composite(first, rest))
}
