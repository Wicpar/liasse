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

/// One seed row staged in phase one: the compiled collection it belongs to and
/// the address its supplied values occupy in the prospective state.
struct Staged<'a> {
    collection: &'a CompiledCollection,
    address: RowAddress,
}

/// Admit every `$data` row into the prospective state, recording touched
/// addresses for the final rule pass.
///
/// §9.1 admits seed data in two phases so member order carries no meaning: first
/// every seeded row identity and supplied value is staged into one prospective
/// state; then each row's defaults and normalization resolve against that whole
/// state — so a default reading another seeded collection (`count(/companies)`)
/// observes it regardless of the order the two appear in `$data`.
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
    let mut staged: Vec<Staged<'_>> = Vec::new();
    // Phase one: stage every seed row's identity and supplied values (no defaults
    // yet), building the prospective state and the ordered work list.
    for member in collections {
        if let Some(collection) = compiled.collection(&member.name.text) {
            let store_path = CollectionPath::top(NameSegment::new(member.name.text.clone()));
            stage_rows(ctx, prospective, &mut staged, collection, &store_path, &member.value)?;
            continue;
        }
        // §8.2/§9.1: a `$data` member naming a singleton root field seeds that
        // field; an unknown or computed member is not seedable. §4.2/C.4: the value
        // (and each static-struct member) is a literal-or-expression position.
        if let Some(node) = model.root().member(&member.name.text).map(|m| &m.node)
            && crate::singleton::member_type(model, node).is_some()
        {
            let value = crate::seed_value::materialize_singleton(
                model, node, &member.name.text, &member.value, ctx, prospective,
            )?;
            singleton.insert(member.name.text.clone(), value);
        }
    }
    if !singleton.is_empty() {
        prospective.insert(crate::singleton::address(), singleton);
    }
    apply_singleton_defaults(compiled, ctx, prospective)?;
    apply_singleton_normalizes(compiled, ctx, prospective)?;
    // Phase two: with the full prospective state in place, resolve each staged
    // row's defaults and normalization against it (§9.1 "defaults are then
    // evaluated by dependency").
    for entry in &staged {
        let mut fields = prospective.get(&entry.address).cloned().unwrap_or_else(FieldMap::new);
        rules::apply_defaults(entry.collection, &mut fields, ctx, prospective)?;
        rules::normalize_all(entry.collection, &mut fields, ctx, prospective)?;
        prospective.replace(&entry.address, fields);
        touched.push(entry.address.clone());
    }
    Ok(())
}

/// Stage every seed row of the collection at `store_path` (top-level or nested,
/// §5.4) into the prospective state with its supplied values only, then recurse
/// into each row's nested-collection members (§5.5). The address of each row
/// roots the store path of its children so a nested pool or spend keeps its
/// ancestor identity (§15). Defaults resolve later, in phase two.
fn stage_rows<'a>(
    ctx: &EvalCtx<'_>,
    prospective: &mut Prospective,
    staged: &mut Vec<Staged<'a>>,
    collection: &'a CompiledCollection,
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
        let fields = decode_row(ctx, prospective, collection, &entry.name.text, &entry.value)?;
        // A seed row's key fields come from the map key (§9.1), so the address is
        // known before any default resolves.
        let key = row_key(collection, &fields)?;
        let address = store_path.row(key);
        if prospective.contains(&address) {
            return Err(Rejection::new(RejectionReason::DuplicateKey, "duplicate seed key")
                .at(address.render()));
        }
        prospective.insert(address.clone(), fields);
        staged.push(Staged { collection, address: address.clone() });
        // §5.5: a seed row may carry nested-collection initializers, staged under
        // the parent address through the same pipeline.
        let members = doc::object(&entry.value).into_iter().flatten();
        for member in members {
            if let Some(child) = collection.child(&member.name.text) {
                let child_path = CollectionPath::nested(
                    address.steps().cloned(),
                    NameSegment::new(member.name.text.clone()),
                );
                stage_rows(ctx, prospective, staged, child, &child_path, &member.value)?;
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
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
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
        let key_component = key_components.iter().find(|(name, _)| *name == &field.name);
        let value = match supplied {
            // §4.2/C.4: a `$data` value is a literal-or-expression position — honor
            // the leading-`'` literal escape and the leading-`=` expression form
            // (against the fields decoded so far as `.` and the staged seed state).
            Some(member) => {
                let value = crate::seed_value::materialize(
                    &field.ty, &field.name, &member.value, collection, &fields, ctx, prospective,
                )?;
                // §9.1: "The map member supplies the local key. A repeated key
                // field MUST agree with it." When a key field is supplied in the
                // row body, its value MUST equal the component the `$data` map
                // member key decodes to; a disagreement is an admission-class key
                // fault that rejects the whole atomic load (§9.3/§9.4).
                if let Some((_, component)) = key_component {
                    let from_key =
                        decode(&field.ty, &serde_json::Value::String(component.clone()), &field.name)?;
                    if value != from_key {
                        return Err(Rejection::new(
                            RejectionReason::DuplicateKey,
                            format!(
                                "repeated key field '{}' disagrees with the seed map member key: \
                                 the map member supplies `{}` but the row body supplies `{}`",
                                field.name,
                                from_key.to_canonical_json_string(),
                                value.to_canonical_json_string(),
                            ),
                        ));
                    }
                }
                value
            }
            None => match key_component {
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

/// Apply the insertion default of each writable singleton root field that no
/// `$data` value supplied (§8.2), the singleton analogue of a collection field
/// default. Supplied values are already staged, so a default may read a sibling
/// singleton member or a staged collection identity. Runs at every genesis load,
/// whether or not the package declares `$data`.
pub(crate) fn apply_singleton_defaults(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &mut Prospective,
) -> Result<(), Rejection> {
    let root_address = crate::singleton::address();
    for def in &compiled.root_singleton_defaults {
        if prospective.get(&root_address).is_some_and(|f| f.contains_key(&def.name)) {
            continue;
        }
        let root = liasse_expr::Cell::Row(Box::new(ctx.root(prospective)));
        let value = default_scalar(ctx.eval(prospective, &def.default, &root)?)?;
        let mut fields = prospective.get(&root_address).cloned().unwrap_or_else(FieldMap::new);
        fields.insert(def.name.clone(), value);
        prospective.insert(root_address.clone(), fields);
    }
    Ok(())
}

/// Normalize every writable singleton root member that carries a `$normalize`
/// and holds a value (§8.2/§8.8/§9.1): a seeded or defaulted singleton member
/// passes through the same normalization a collection seed row does. Runs after
/// [`apply_singleton_defaults`] so a defaulted value is normalized too. An
/// absent member (no seed, no default) is left absent rather than normalizing
/// `none`, matching how the singleton root materializes an unwritten member.
pub(crate) fn apply_singleton_normalizes(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &mut Prospective,
) -> Result<(), Rejection> {
    let root_address = crate::singleton::address();
    let Some(mut fields) = prospective.get(&root_address).cloned() else { return Ok(()) };
    let mut changed = false;
    for norm in &compiled.root_singleton_normalizes {
        if fields.contains_key(&norm.name) {
            rules::normalize_singleton_field(compiled, &norm.name, &mut fields, ctx, prospective)?;
            changed = true;
        }
    }
    if changed {
        prospective.replace(&root_address, fields);
    }
    Ok(())
}

/// The scalar value a singleton default evaluates to. A single-row result yields
/// its key (a ref default, §5.6); a collection result is not a scalar.
fn default_scalar(cell: liasse_expr::Cell) -> Result<liasse_value::Value, Rejection> {
    match cell {
        liasse_expr::Cell::Scalar(value) => Ok(value),
        liasse_expr::Cell::Row(row) => Ok(row.key().clone()),
        liasse_expr::Cell::Collection(_) => Err(Rejection::new(
            RejectionReason::TypeError,
            "a singleton root default must evaluate to a scalar value",
        )),
    }
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
