//! Genesis seed admission (§9.1): `$data` rows pass through the same defaults,
//! normalization, checks, and key/ref/uniqueness rules as mutation inserts.
//!
//! CORE scope covers keyed collections whose rows carry scalar, ref, and set
//! fields, keyed by a single `$key` field — including nested keyed collections in
//! a seed row (§5.4/§5.5), which a meter's pool/spend arrangement (§15) seeds
//! under an ancestor row. Composite seed keys remain a documented seam.

use liasse_ident::{KeyText, NameSegment};
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

/// Decode a seed row's declared fields against their types, supplying each key
/// field from the map member when the row omits it (§9.1). The member name is
/// the D.2-escaped key text; for a composite key it joins one component per
/// `$key` field (in key order), so an omitted key field takes its own decoded
/// component — not the whole joined text.
fn decode_row(
    collection: &CompiledCollection,
    key_text: &str,
    row: &DocValue,
) -> Result<FieldMap, Rejection> {
    let members = doc::object(row)
        .ok_or_else(|| Rejection::new(RejectionReason::Malformed, "a seed row must be an object"))?;
    let key_components = decode_key_components(collection, key_text)?;
    let mut fields = FieldMap::new();
    for field in &collection.fields {
        let supplied = members.iter().find(|m| m.name.text == field.name);
        let value = match supplied {
            Some(member) => decode(&field.ty, &doc::to_json(&member.value), &field.name)?,
            None => match key_components.iter().find(|(name, _)| *name == &field.name) {
                Some((_, component)) => {
                    decode(&field.ty, &serde_json::Value::String(component.clone()), &field.name)?
                }
                None => continue,
            },
        };
        fields.insert(field.name.clone(), value);
    }
    // §5.3: a supplied static-struct member decodes its inner fields onto a
    // `struct` value stored on the row, so a view/sort over the struct reads its
    // components (B.4). Omitted-member struct defaults remain a documented seam.
    for struct_meta in &collection.structs {
        if let Some(member) = members.iter().find(|m| m.name.text == struct_meta.name) {
            fields.insert(struct_meta.name.clone(), decode_struct(struct_meta, &member.value)?);
        }
    }
    Ok(fields)
}

/// Decode a supplied static-struct initializer (§5.3) into a `struct` value: each
/// declared struct field decodes its supplied member against its type; an omitted
/// member stays absent (its default resolution is a documented seam).
fn decode_struct(
    struct_meta: &crate::compiled::CompiledStruct,
    body: &DocValue,
) -> Result<liasse_value::Value, Rejection> {
    let members = doc::object(body).ok_or_else(|| {
        Rejection::new(RejectionReason::Malformed, format!("struct `{}` must be an object", struct_meta.name))
    })?;
    let mut entries = Vec::new();
    for field in &struct_meta.fields {
        if let Some(member) = members.iter().find(|m| m.name.text == field.name) {
            let value = decode(&field.ty, &doc::to_json(&member.value), &field.name)?;
            entries.push((liasse_value::Text::new(field.name.clone()), value));
        }
    }
    Ok(liasse_value::Value::Struct(liasse_value::Struct::new(entries)))
}

/// The decoded (unescaped) key-text component for each `$key` field, in key
/// order (§5.4, D.2): the member name is split on the unescaped `:` join
/// separators and each component decoded, so a composite key supplies one value
/// per key field and any escaped `:`/`/`/`%` inside a component survives.
fn decode_key_components<'a>(
    collection: &'a CompiledCollection,
    key_text: &str,
) -> Result<Vec<(&'a String, String)>, Rejection> {
    let malformed = || Rejection::new(RejectionReason::Malformed, "seed key text is malformed");
    let components = KeyText::parse(key_text.to_owned())
        .and_then(|text| text.components())
        .map_err(|_| malformed())?;
    if components.len() != collection.key.len() {
        return Err(malformed());
    }
    Ok(collection
        .key
        .iter()
        .zip(components)
        .map(|(name, component)| (name, component.as_str().to_owned()))
        .collect())
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
