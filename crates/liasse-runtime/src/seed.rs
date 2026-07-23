//! Genesis seed admission (§9.1): `$data` rows pass through the same defaults,
//! normalization, checks, and key/ref/uniqueness rules as mutation inserts.
//!
//! CORE scope covers keyed collections whose rows carry scalar, ref, and set
//! fields, keyed by a single `$key` field — including nested keyed collections in
//! a seed row (§5.4/§5.5), which a meter's pool/spend arrangement (§15) seeds
//! under an ancestor row. Composite seed keys remain a documented seam.

use std::collections::BTreeMap;

use liasse_ident::{KeyText, NameSegment};
use liasse_syntax::DocValue;
use liasse_store::{CollectionPath, KeyValue, RowAddress};
use liasse_value::{Type, Value};

use crate::compiled::{Compiled, CompiledCollection, CompiledDefault};
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

/// How a seed row whose address already holds a value is treated (§9.1 vs §13.13).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeedMode {
    /// Genesis / installation (§9.1): every seed identity is fresh, so a
    /// collision with an already-staged row is a duplicate-key fault, and
    /// singleton `$data` members seed the reserved §8.2 root row.
    Genesis,
    /// Update apply-if-absent (§13.13/§4.1): a seed applies only where its
    /// address holds no current value — an ABSENT address is inserted, an
    /// occupied one is RETAINED unchanged (never overwritten). The §8.2 singleton
    /// is already carried by the §20.1 copy, so singleton `$data` on update stays
    /// with that carry (a documented seam); only keyed-collection rows reconcile
    /// here.
    ApplyIfAbsent,
    /// Installation `$data` overlay (§13.3): applied onto an instance the package
    /// `$data` seed already loaded. An ABSENT address is inserted like a fresh
    /// genesis row; an OCCUPIED one is a three-way MERGE onto the seeded row —
    /// writable scalar and struct fields overlay, `$set` fields union, and nested
    /// keyed child collections merge by key (the recursion carries this mode down).
    /// Every resulting row is re-staged so it passes ordinary insertion and load
    /// validation (§13.3), unlike the retain-only [`ApplyIfAbsent`](Self::ApplyIfAbsent).
    Overlay,
}

/// Admit every `$data` row into the prospective state, recording touched
/// addresses for the final rule pass.
///
/// §9.1 admits seed data in two phases so member order carries no meaning: first
/// every seeded row identity and supplied value is staged into one prospective
/// state; then each row's defaults and normalization resolve against that whole
/// state — so a default reading another seeded collection (`count(/companies)`)
/// observes it regardless of the order the two appear in `$data`.
///
/// `mode` selects the collision policy (§9.1 genesis vs §13.13 update
/// apply-if-absent). On the update path an occupied address is retained rather
/// than rejected, and the §8.2 singleton — already carried by the §20.1 copy — is
/// left untouched (a documented seam), so only keyed-collection rows reconcile.
pub(crate) fn admit(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &mut Prospective,
    touched: &mut Vec<RowAddress>,
    data: &DocValue,
    mode: SeedMode,
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
            stage_rows(ctx, prospective, &mut staged, collection, &store_path, &member.value, mode)?;
            continue;
        }
        // §8.2/§9.1: a `$data` member naming a singleton root field seeds that
        // field; an unknown or computed member is not seedable. §4.2/C.4: the value
        // (and each static-struct member) is a literal-or-expression position. On
        // update the §20.1 singleton copy owns the reserved row, so a singleton
        // `$data` member is left to that carry (a documented §13.13 seam).
        if mode == SeedMode::Genesis
            && let Some(node) = model.root().member(&member.name.text).map(|m| &m.node)
            && crate::singleton::member_type(model, node).is_some()
        {
            let value = crate::seed_value::materialize_singleton(
                model, node, &member.name.text, &member.value, ctx, prospective,
            )?;
            singleton.insert(member.name.text.clone(), value);
        }
    }
    // §8.2 singleton seeding/defaulting/normalization runs only at genesis; on the
    // update path the §20.1 singleton copy and its `apply_singleton_*` passes in
    // `build_migrated` already staged the reserved row.
    if mode == SeedMode::Genesis {
        if !singleton.is_empty() {
            prospective.insert(crate::singleton::address(), singleton);
        }
        apply_singleton_defaults(compiled, ctx, prospective)?;
        apply_singleton_normalizes(compiled, ctx, prospective)?;
    }
    // Phase two: with the full prospective state in place, resolve each staged
    // row's defaults and normalization against it (§9.1 "defaults are then
    // evaluated by dependency").
    for entry in &staged {
        let mut fields = prospective.get(&entry.address).cloned().unwrap_or_else(FieldMap::new);
        // §5.1/§8.12: each seeded row draws its own generation, so a `uuid()`
        // default seeding a key is fresh per row across the genesis load, while a
        // state-derived default still observes the whole seed set (SPEC-ISSUES
        // item 4). §9.1's single atomic seed load does not subdivide those reads.
        let generation = prospective.next_generation();
        // §9.1/§5.4: phase one already staged this row's nested keyed collections
        // in the prospective state, so passing its address lets a default reading a
        // computed that aggregates a nested child (`count(.items)`) observe them.
        rules::apply_defaults(entry.collection, &mut fields, ctx, prospective, generation, Some(&entry.address))?;
        rules::normalize_all(entry.collection, &mut fields, ctx, prospective)?;
        // §9.1: a seed row "passes through the same defaults, normalization, checks,
        // key, ref, uniqueness, bucket, and meter rules as mutation inserts", so it
        // shares the mutation insert's final field-write boundary (§5.9/§22.5/§A.5):
        // every enum leaf re-validates to a positioned `Value::Enum` and every
        // `timestamp` field value rescales to the field's declared precision under
        // the package rounding (§4.4). A supplied literal already decodes canonical,
        // making this a no-op for it; a defaulted enum/`now()` value — produced by
        // `apply_defaults` at the clock/text form — is coerced here exactly as the
        // `+`/bulk-insert path coerces it, so a seeded row never stores a raw enum
        // label or a finer-precision timestamp than a mutation-inserted one would.
        rules::coerce_fields(entry.collection, &mut fields, &entry.collection.name, compiled.division_rounding)?;
        prospective.replace(&entry.address, fields);
        touched.push(entry.address.clone());
    }
    Ok(())
}

/// §13.13: reconcile the target package's `$bundle` against the migrated instance
/// state as a three-way merge among the OLD package bundle, the NEW package bundle,
/// and the current state. A row newly present in the new bundle is inserted; a
/// bundled field whose current value still equals the old bundle value takes the new
/// bundle value, otherwise the current (locally modified) value is retained; a row
/// removed from the new bundle is deleted only when its current subtree still equals
/// the old bundled subtree, otherwise it is retained as local data.
///
/// CORE scope is top-level keyed collections with scalar/struct fields, which the
/// same [`decode_row`] and [`admit`] machinery already covers. Nested keyed
/// collections and set-membership reconciliation apply the same rule recursively
/// (§13.13); they are a documented seam no CORE bundle address reaches.
pub(crate) fn merge_bundle(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &mut Prospective,
    touched: &mut Vec<RowAddress>,
    old_bundle: Option<&DocValue>,
    new_bundle: &DocValue,
) -> Result<(), Rejection> {
    // A row newly present in the new bundle is inserted; a row already present is
    // left in place here and reconciled field-by-field below (never overwritten
    // wholesale, so a locally modified row keeps its edits).
    admit(compiled, ctx, prospective, touched, new_bundle, SeedMode::ApplyIfAbsent)?;
    let Some(new_cols) = doc::object(new_bundle) else { return Ok(()) };
    for member in new_cols {
        let Some(collection) = compiled.collection(&member.name.text) else { continue };
        let store_path = CollectionPath::top(NameSegment::new(member.name.text.clone()));
        let new_rows = decode_bundle_rows(ctx, prospective, collection, &store_path, &member.value)?;
        let old_rows = match old_bundle_collection(old_bundle, &member.name.text) {
            Some(rows) => decode_bundle_rows(ctx, prospective, collection, &store_path, rows)?,
            None => BTreeMap::new(),
        };
        // §13.13: replace each present row's bundled fields under the three-way rule.
        for (address, new_fields) in &new_rows {
            let Some(current) = prospective.get(address) else { continue };
            let merged = merge_row_fields(current, old_rows.get(address), new_fields);
            prospective.replace(address, merged);
            touched.push(address.clone());
        }
        // §13.13: a row the new bundle dropped is deleted only when its current
        // subtree still equals the old bundled subtree; a locally modified row (or one
        // the new bundle still carries) is retained.
        for (address, old_fields) in &old_rows {
            if !new_rows.contains_key(address) && prospective.get(address) == Some(old_fields) {
                prospective.remove(address);
            }
        }
    }
    Ok(())
}

/// §13.13 per-field rule: the new bundle value applies where the current value still
/// equals the old bundle value; otherwise the current (locally modified) value is
/// retained. Fields the new bundle does not mention are kept as-is.
fn merge_row_fields(current: &FieldMap, old: Option<&FieldMap>, new: &FieldMap) -> FieldMap {
    let mut merged = current.clone();
    for (field, new_value) in new {
        if current.get(field) == old.and_then(|fields| fields.get(field)) {
            merged.insert(field.clone(), new_value.clone());
        }
    }
    merged
}

/// Decode a bundle collection's rows into their addresses and supplied field maps
/// (the same decode a seed row uses), for the §13.13 three-way comparison.
fn decode_bundle_rows(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    collection: &CompiledCollection,
    store_path: &CollectionPath,
    rows: &DocValue,
) -> Result<BTreeMap<RowAddress, FieldMap>, Rejection> {
    let Some(entries) = doc::object(rows) else {
        return Err(Rejection::new(
            RejectionReason::Malformed,
            format!("`$bundle.{}` must map keys to rows", collection.name),
        ));
    };
    let mut out = BTreeMap::new();
    for entry in entries {
        let fields = decode_row(ctx, prospective, collection, &entry.name.text, &entry.value)?;
        let key = row_key(collection, &fields)?;
        out.insert(store_path.row(key), fields);
    }
    Ok(out)
}

/// The old package bundle's rows object for the collection named `name`, when the
/// prior release bundled it.
fn old_bundle_collection<'a>(old_bundle: Option<&'a DocValue>, name: &str) -> Option<&'a DocValue> {
    doc::object(old_bundle?)?.iter().find(|member| member.name.text == name).map(|member| &member.value)
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
    mode: SeedMode,
) -> Result<(), Rejection> {
    let Some(entries) = doc::object(rows) else {
        return Err(Rejection::new(
            RejectionReason::Malformed,
            format!("`$data.{}` must map keys to rows", collection.name),
        ));
    };
    for entry in entries {
        let decoded = decode_row(ctx, prospective, collection, &entry.name.text, &entry.value)?;
        // A seed row's key fields come from the map key (§9.1), so the address is
        // known before any default resolves.
        let key = row_key(collection, &decoded)?;
        let address = store_path.row(key);
        let fields = if prospective.contains(&address) {
            match mode {
                // §9.1: two seed rows at one key is a duplicate-key fault.
                SeedMode::Genesis => {
                    return Err(Rejection::new(RejectionReason::DuplicateKey, "duplicate seed key")
                        .at(address.render()));
                }
                // §13.13/§4.1 apply-if-absent: the occupied address keeps its
                // current value — the seed neither overwrites it nor re-stages it,
                // and its nested initializers are left to the retained row.
                SeedMode::ApplyIfAbsent => continue,
                // §13.3 overlay: an occupied address is a three-way merge — the
                // overlay's writable scalar/struct fields replace the seeded ones,
                // `$set` fields union, and fields the overlay omits are retained. The
                // merged row is re-staged so phase two re-resolves its defaults and
                // normalization and `finalize` re-checks it (ordinary insertion
                // validation); its nested child collections merge below under this
                // same mode.
                SeedMode::Overlay => {
                    let existing = prospective.get(&address).cloned().unwrap_or_else(FieldMap::new);
                    overlay_fields(collection, existing, decoded)
                }
            }
        } else {
            decoded
        };
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
                stage_rows(ctx, prospective, staged, child, &child_path, &member.value, mode)?;
            }
        }
    }
    Ok(())
}

/// Merge a §13.3 installation-`$data` overlay row (`overlay`) onto the row already
/// seeded at the same key (`existing`). Each field the overlay supplies replaces
/// the seeded value, except a `$set` field, which unions its members onto the
/// current set (§13.3 "unions sets"); a field the overlay omits is retained. Key
/// fields agree by construction (the two rows share an address), so replacing them
/// is a no-op. Nested keyed child collections are not folded here — the caller's
/// recursion merges each under this row's address through the same
/// [`SeedMode::Overlay`] pipeline (§13.3 "merges keyed child collections by key").
fn overlay_fields(collection: &CompiledCollection, mut existing: FieldMap, overlay: FieldMap) -> FieldMap {
    for (name, value) in overlay {
        let is_set = collection.fields.iter().any(|field| field.name == name && matches!(field.ty, Type::Set(_)));
        let merged = match (is_set, existing.get(&name)) {
            (true, Some(Value::Set(current))) => match value {
                Value::Set(incoming) => {
                    let mut union = current.clone();
                    union.extend(incoming);
                    Value::Set(union)
                }
                other => other,
            },
            _ => value,
        };
        existing.insert(name, merged);
    }
    existing
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
/// **optional** member is filled with `Value::None` (A.1) so every struct value of
/// a given shape carries the same member set. The wire decode path already does
/// this (`liasse_value::decode`), and Annex B.4 ordering depends on it: a present
/// member must precede an absent one, which the derived `Struct` `Ord` realizes
/// only when the absent member is materialized as `Value::None` (whose rank is the
/// maximum, B.2). An omitted **non-optional** member's default resolution remains a
/// documented seam (left absent here).
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
        } else if matches!(field.ty, Type::Optional(_)) {
            entries.push((liasse_value::Text::new(field.name.clone()), liasse_value::Value::None));
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
        let value = match &def.default {
            // §4.2/§C.4: a literal singleton default is decoded at compile.
            CompiledDefault::Literal(value) => value.clone(),
            CompiledDefault::Expr(typed) => {
                let root = liasse_expr::Cell::Row(Box::new(ctx.root(prospective)));
                default_scalar(ctx.eval(prospective, typed, &root)?)?
            }
        };
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
