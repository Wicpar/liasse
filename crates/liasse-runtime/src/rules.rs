//! The admission rule pipeline (§5, §8.8): insertion defaults by declaration,
//! normalization, and — over the final prospective state — field/row checks,
//! reference integrity, and additional uniqueness.
//!
//! Defaults resolve in declaration order (a documented CORE simplification of
//! the full dependency ordering §5.1 permits any topological evaluation of).
//! Every rejection leaves the prospective state to be discarded whole, so the
//! prior committed state is never touched.

use liasse_expr::{Cell, DivisionRounding};
use liasse_ident::NameSegment;
use liasse_store::{CollectionPath, RowAddress};
use liasse_value::{Precision, RefKey, Struct, StructType, Text, Value};

use crate::compiled::{Compiled, CompiledCollection, CompiledField, CompiledStruct};
use crate::error::{Rejection, RejectionReason};
use crate::eval::EvalCtx;
use crate::generator::Generation;
use crate::materialize::FieldMap;
use crate::refid::{identity_of, ref_identity};
use crate::state::Prospective;

/// Resolve insertion defaults for the omitted fields of a new row (§5.1), then
/// fill any still-absent declared field with `none`, so the row is complete.
///
/// `generation` is the admitted row occurrence's generated-value ordinal
/// (SPEC-ISSUES item 4, §5.1/§8.12): every default of this one row evaluates at
/// it, so two `uuid()` defaults of the row stay distinct by call site while the
/// *same* `uuid()` default across two rows of one request — each drawing its own
/// generation — stays distinct by generation. A state-derived default
/// (`count(/coll) + 1`) is unaffected: it still observes the same pre-statement
/// state every row of one bulk insertion sees (§5.1).
///
/// `address` is the row's staged prospective address when one exists (the
/// genesis-seed phase-two path, where every seeded row — including this row's
/// nested collections — is already staged before defaults resolve, §9.1), so a
/// default reading a computed that aggregates a nested keyed collection observes
/// the materialized nested rows. It is `None` on the `+`/bulk-insert path, whose
/// nested initializers stage only after the parent's defaults resolve — §5.1's
/// rows of one statement become selectable only once all resolve.
pub(crate) fn apply_defaults(
    collection: &CompiledCollection,
    fields: &mut FieldMap,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    generation: Generation,
    address: Option<&RowAddress>,
) -> Result<(), Rejection> {
    for field in &collection.fields {
        if fields.contains_key(&field.name) {
            continue;
        }
        if let Some((typed, _)) = &field.default {
            // §5.1: defaults and computed insertion values form one dependency
            // graph, so a default MAY read a computed value (`booked: "int = .tax
            // + 1"` over `tax: "= ..."`). Expose the provisional row with its
            // computed values folded in (dependency order) AND its already-staged
            // nested keyed collections grafted (§5.4), exactly as a row/struct
            // `$check` does, so the default reads `.tax` — or a computed
            // aggregating a nested child — instead of faulting on a missing member.
            // `provisional_row_cell` degenerates to the bare row cell when there is
            // no staged nested collection to expose.
            let current = ctx.provisional_row_cell(prospective, collection, address, fields);
            let value = scalar(ctx.eval_generative(prospective, typed, &current, generation)?);
            fields.insert(field.name.clone(), value);
        }
    }
    for field in &collection.fields {
        fields.entry(field.name.clone()).or_insert_with(|| absent_value(&field.ty));
    }
    Ok(())
}

/// The value of a declared field that was neither supplied nor defaulted (§5.1,
/// §5.5): an omitted `$set` starts empty, an omitted non-optional `map` starts
/// empty (the set-analogous default; SPEC-ISSUES item 37), and every other
/// omitted field reads `none`. A distinct empty set/map (not `none`) is what
/// makes an omitted collection project as `[]` — an empty collection is the
/// declared shape holding (§22.1) — and lets a later `+`/`-` (a `map` entry
/// write) act against the existing membership rather than against `none`.
fn absent_value(ty: &liasse_value::Type) -> Value {
    empty_container(ty).unwrap_or(Value::None)
}

/// The §5.5 empty-container default a non-optional `set`/`map` takes when omitted —
/// an empty set or empty map — or `None` for any other declared type (a scalar, an
/// `optional`, a `struct`), which carries no container default. This is the single
/// seam the "row OR struct" empty-container rule flows through: the row field
/// absent-fill ([`absent_value`], reached from [`apply_defaults`]) and the static
/// struct member fill ([`complete_struct_containers`]) both resolve their omitted
/// containers here, so the two paths cannot drift.
fn empty_container(ty: &liasse_value::Type) -> Option<Value> {
    match ty {
        liasse_value::Type::Set(_) => Some(Value::Set(std::collections::BTreeSet::new())),
        liasse_value::Type::Map(..) => Some(Value::Map(std::collections::BTreeMap::new())),
        _ => None,
    }
}

/// Complete a freshly built static struct with its §5.5/§22.1 container defaults so
/// the declared shape holds in every committed state (§5.3): every omitted
/// non-optional `set`/`map` member starts as the empty container — the "row OR
/// struct" default, symmetric to the row field absent-fill [`apply_defaults`] runs
/// — and every supplied nested static struct is recursed into so a struct-in-struct
/// completes its own containers too. An omitted optional member stays absent: `none`
/// is absence, never a stored value (A.1). Only the container/struct shape is
/// completed; a supplied scalar member is untouched. Run after the struct's explicit
/// member defaults and before its `$check`, so the check sees the completed shape
/// (§5.10).
pub(crate) fn complete_struct_containers(fields: &mut FieldMap, struct_fields: &[CompiledField]) {
    for field in struct_fields {
        match fields.get_mut(&field.name) {
            // A supplied nested static struct completes its own containers; any other
            // supplied member keeps its value verbatim.
            Some(value) => {
                if let liasse_value::Type::Struct(inner) = &field.ty {
                    let owned = std::mem::replace(value, Value::None);
                    *value = complete_nested_struct(inner, owned);
                }
            }
            // An omitted non-optional set/map starts empty; anything else stays absent.
            None => {
                if let Some(container) = empty_container(&field.ty) {
                    fields.insert(field.name.clone(), container);
                }
            }
        }
    }
}

/// Recurse the §5.5 container default into a nested static-struct value, which a
/// struct carries only as a `Type::Struct` (not a compiled struct), so
/// [`complete_struct_containers`] cannot reach its members directly. Applies the
/// identical rule one level down: a present nested struct recurses, an omitted
/// non-optional `set`/`map` member starts empty, an omitted optional stays absent.
/// A non-struct value — the documented view/ref struct-initializer seam — is
/// returned unchanged. Rebuilt in the same style as [`coerce_value`]'s struct arm.
fn complete_nested_struct(struct_ty: &StructType, value: Value) -> Value {
    let Value::Struct(existing) = value else { return value };
    // Keep every present member, recursing into a nested static struct.
    let mut members: Vec<(Text, Value)> = existing
        .fields()
        .map(|(name, member)| {
            let completed = match struct_ty.field(name.as_str()) {
                Some(liasse_value::Type::Struct(inner)) => complete_nested_struct(inner, member.clone()),
                _ => member.clone(),
            };
            (name.clone(), completed)
        })
        .collect();
    // Add the empty container for every omitted non-optional set/map member.
    for (name, member_ty) in struct_ty.fields() {
        if members.iter().all(|(present, _)| present.as_str() != name.as_str())
            && let Some(container) = empty_container(member_ty)
        {
            members.push((Text::new(name.as_str()), container));
        }
    }
    Value::Struct(Struct::new(members))
}

/// Coerce every present enum-typed field value to a validated enum label (§5.9):
/// the `$enum` array is a closed set, so a supplied value whose label is not
/// declared rejects the transition, and an accepted label is carried as a
/// positioned [`Value::Enum`] so ordering (B.1) and equality follow declaration
/// order rather than raw text. A value that already carries an enum position is
/// re-validated against the *current* declared set (not blindly trusted), so a
/// migrated value stranded by a narrowed closed set rejects (§20.1/§22.1);
/// `none` (an absent optional) is untouched.
///
/// This is also the timestamp field-write boundary (§22.5/§A.5): a `timestamp`
/// field value (a `now()` default, a supplied wire count) is rescaled to the
/// field's declared fractional-second precision, rounding a halfway value away
/// from zero, so a coarser-precision field never stores a finer-precision count.
pub(crate) fn coerce_fields(
    collection: &CompiledCollection,
    fields: &mut FieldMap,
    where_path: &str,
    rounding: DivisionRounding,
) -> Result<(), Rejection> {
    for field in &collection.fields {
        if enum_of(&field.ty).is_none() && timestamp_precision(&field.ty).is_none() {
            continue;
        }
        let Some(value) = fields.get(&field.name) else { continue };
        let coerced = coerce_value(&field.ty, value, &field.name, where_path, rounding)?;
        fields.insert(field.name.clone(), coerced);
    }
    Ok(())
}

/// Coerce an application-visible key operand's enum leaves to positioned enum
/// values against the collection's declared key-field types (§5.9/§5.4/§8.5).
///
/// A delete or keyed selector supplies an enum key component as its bare `text`
/// label — a call argument is untyped text (`del(status: "archived")`), and the
/// object/list operand is evaluated structurally, so no per-field coercion (the
/// one an add's field write applies) ever reaches it. The stored row keys on the
/// positioned [`Value::Enum`], and `Value::Text("archived") != Value::Enum(..)`,
/// so an un-coerced operand addresses NO row and the delete silently no-ops even
/// against a live row — the row becomes unaddressable by its own key. Coercing the
/// operand here makes the lookup key identical to the stored key.
///
/// A single-field key coerces its lone scalar; a composite key coerces each
/// component in `$key` order (a struct-key member reaches its enum leaf through
/// [`coerce_value`]'s descent). A component whose label the current enum does not
/// declare is left unchanged — it can match no live row, so the operation no-ops
/// exactly as removing an absent row does (§8.5), rather than rejecting.
pub(crate) fn coerce_key_operand(
    collection: &CompiledCollection,
    key: Value,
    where_path: &str,
    rounding: DivisionRounding,
) -> Value {
    match collection.key.as_slice() {
        [name] => coerce_key_component(collection, name, key, where_path, rounding),
        names => match key {
            Value::Composite(components) if components.len() == names.len() => Value::Composite(
                names
                    .iter()
                    .zip(components)
                    .map(|(name, component)| coerce_key_component(collection, name, component, where_path, rounding))
                    .collect(),
            ),
            other => other,
        },
    }
}

/// Coerce one key component against the declared type of key field `name` — a
/// top-level field or a static-struct key member — leaving it unchanged when the
/// type carries no enum or the label is not currently declared (a no-op lookup).
fn coerce_key_component(
    collection: &CompiledCollection,
    name: &str,
    value: Value,
    where_path: &str,
    rounding: DivisionRounding,
) -> Value {
    let ty = collection.field(name).map(|field| field.ty.clone()).or_else(|| collection.struct_type(name));
    match ty {
        Some(ty) if contains_enum(&ty) => coerce_value(&ty, &value, name, where_path, rounding).unwrap_or(value),
        _ => value,
    }
}

/// Coerce a value against a field type by re-validating every enum leaf it
/// carries against that enum's *current* declared label set (§5.9), returning the
/// value unchanged when the type has no enum leaf. A supplied `text`/`enum` leaf
/// is parsed against the closed set; an undeclared label rejects.
///
/// An already-positioned [`Value::Enum`] is NOT blindly trusted: its label — the
/// wire identity of an enum value (A.1) — is re-resolved against the current
/// declared set. A label the current enum still declares re-derives its
/// declaration-order ordinal (so a reorder settles on the current position),
/// while a label the set no longer declares is out of domain and rejects. This is
/// what makes the §20.1 compatible same-identity copy of an enum value fail when a
/// narrowing release drops its label, rather than stranding an undeclared label in
/// committed target state (§22.1 field types).
///
/// The check DESCENDS into containers — `optional`, `set`, `struct`, `map`, and
/// composite — so an enum nested any number of layers down (a struct field, a
/// set/map element, a struct-in-struct) is re-validated exactly like a top-level
/// enum field (§20.1/§22.1). Re-deriving a leaf's ordinal can move it, so a
/// touched container is rebuilt from its re-validated members; a `set`/`map`
/// re-sorts under the new ordering. Members whose declared type carries no enum —
/// and any member the target type no longer declares — are copied verbatim, so the
/// walk changes nothing but enum leaves.
pub(crate) fn coerce_value(
    ty: &liasse_value::Type,
    value: &Value,
    field_name: &str,
    where_path: &str,
    rounding: DivisionRounding,
) -> Result<Value, Rejection> {
    use liasse_value::{Struct, Type};
    match ty {
        Type::Enum(enum_ty) => coerce_enum_leaf(enum_ty, value, field_name, where_path),
        // §22.5/§A.5: convert a written `timestamp` value to the field's declared
        // precision, rounding a coarser conversion under the package-selected mode
        // (§4.4). A `none` (absent optional) is untouched; any non-timestamp value
        // is left to the ordinary assignability check.
        Type::Timestamp(target) => Ok(rescale_timestamp(value, *target, rounding)),
        // An `optional` around an enum leaf: `none` is absence (untouched); a
        // present value re-validates against the inner type.
        Type::Optional(inner) => match value {
            Value::None => Ok(Value::None),
            _ => coerce_value(inner, value, field_name, where_path, rounding),
        },
        Type::Set(inner) if contains_enum(inner) => {
            let Value::Set(members) = value else { return Ok(value.clone()) };
            members
                .iter()
                .map(|member| coerce_value(inner, member, field_name, where_path, rounding))
                .collect::<Result<std::collections::BTreeSet<_>, _>>()
                .map(Value::Set)
        }
        Type::Struct(struct_ty) if contains_enum(ty) => {
            let Value::Struct(existing) = value else { return Ok(value.clone()) };
            let mut rebuilt = Vec::new();
            for (name, member) in existing.fields() {
                let coerced = match struct_ty.field(name.as_str()) {
                    Some(member_ty) => coerce_value(member_ty, member, name.as_str(), where_path, rounding)?,
                    None => member.clone(),
                };
                rebuilt.push((name.clone(), coerced));
            }
            Ok(Value::Struct(Struct::new(rebuilt)))
        }
        Type::Map(key_ty, val_ty) if contains_enum(ty) => {
            let Value::Map(entries) = value else { return Ok(value.clone()) };
            entries
                .iter()
                .map(|(key, val)| {
                    Ok::<_, Rejection>((
                        coerce_value(key_ty, key, field_name, where_path, rounding)?,
                        coerce_value(val_ty, val, field_name, where_path, rounding)?,
                    ))
                })
                .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
                .map(Value::Map)
        }
        Type::Composite(components) if contains_enum(ty) => {
            let Value::Composite(values) = value else { return Ok(value.clone()) };
            if values.len() != components.len() {
                return Ok(value.clone());
            }
            components
                .iter()
                .zip(values)
                .map(|((name, comp_ty), comp)| coerce_value(comp_ty, comp, name, where_path, rounding))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Composite)
        }
        _ => Ok(value.clone()),
    }
}

/// Re-validate one enum leaf against its declared label set (§5.9). A `text`/`enum`
/// value is parsed against the closed set; an undeclared label rejects. `none`
/// (an absent optional) is untouched.
fn coerce_enum_leaf(
    enum_ty: &liasse_value::EnumType,
    value: &Value,
    field_name: &str,
    where_path: &str,
) -> Result<Value, Rejection> {
    let label = match value {
        Value::None => return Ok(Value::None),
        Value::Text(text) => text.as_str().to_owned(),
        Value::Enum(existing) => existing.label().to_owned(),
        _ => {
            return Err(Rejection::new(
                RejectionReason::TypeError,
                format!("field `{field_name}` is an enum and takes a declared label"),
            )
            .at(where_path.to_owned()));
        }
    };
    let parsed = enum_ty.parse(&label).map_err(|_| {
        Rejection::new(
            RejectionReason::Evaluation,
            format!(
                "`{label}` is not a declared label of enum field `{field_name}` (accepted: {})",
                enum_ty.labels().join(", "),
            ),
        )
        .at(where_path.to_owned())
    })?;
    Ok(Value::Enum(parsed))
}

/// Whether a field's declared type is an enum (possibly optional). Used by the
/// admission assign path, which coerces a scalar written to a top-level enum field
/// and leaves container-typed assignments to the ordinary assignability check.
#[must_use]
pub(crate) fn is_enum_field(ty: &liasse_value::Type) -> bool {
    enum_of(ty).is_some()
}

/// The enum type a field's declared type resolves to, unwrapping `optional`.
fn enum_of(ty: &liasse_value::Type) -> Option<&liasse_value::EnumType> {
    match ty {
        liasse_value::Type::Enum(en) => Some(en),
        liasse_value::Type::Optional(inner) => enum_of(inner),
        _ => None,
    }
}

/// The declared fractional-second precision of a `timestamp` field, unwrapping
/// `optional`. Gates the timestamp field-write rescale (§22.5/§A.5) so a
/// non-timestamp field is skipped, exactly as [`enum_of`] gates enum coercion.
fn timestamp_precision(ty: &liasse_value::Type) -> Option<Precision> {
    match ty {
        liasse_value::Type::Timestamp(precision) => Some(*precision),
        liasse_value::Type::Optional(inner) => timestamp_precision(inner),
        _ => None,
    }
}

/// Rescale a written timestamp `value` to a field's declared `target` precision
/// (§22.5/§A.5). A non-timestamp value or one already at `target` is returned
/// unchanged; otherwise it is converted through [`liasse_value::Timestamp::to_precision`],
/// rounding a coarser conversion under the package-selected mode (§4.4/§A.5 —
/// half-away-from-zero by default). The engine hands the resolved `rounding`
/// down the field-write boundary so `$semantics.decimal_division.rounding`
/// governs this conversion exactly as it governs decimal `/` and `avg`.
fn rescale_timestamp(value: &Value, target: Precision, rounding: DivisionRounding) -> Value {
    match value {
        Value::Timestamp(timestamp) if timestamp.precision() != target => {
            Value::Timestamp(timestamp.to_precision(target, rounding.mode()))
        }
        _ => value.clone(),
    }
}

/// Whether a declared type carries an enum anywhere — a bare enum, or one nested
/// inside an `optional`, `set`, `struct`, `map`, `view`, or composite. Gates the
/// §20.1 migrated-value re-validation so a container field whose enum leaf a
/// narrowing release stranded is descended into (`coerce_value`), not skipped.
#[must_use]
pub(crate) fn contains_enum(ty: &liasse_value::Type) -> bool {
    use liasse_value::Type;
    match ty {
        Type::Enum(_) => true,
        Type::Optional(inner) | Type::Set(inner) | Type::View(inner) => contains_enum(inner),
        Type::Map(key, val) => contains_enum(key) || contains_enum(val),
        Type::Struct(fields) => fields.fields().any(|(_, member)| contains_enum(member)),
        Type::Composite(components) => components.iter().any(|(_, member)| contains_enum(member)),
        _ => false,
    }
}

/// Normalize every field carrying a `$normalize` expression (§8.8): `.` is the
/// field's own value.
pub(crate) fn normalize_all(
    collection: &CompiledCollection,
    fields: &mut FieldMap,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<(), Rejection> {
    for field in &collection.fields {
        if field.normalize.is_some() {
            normalize_field(collection, &field.name, fields, ctx, prospective)?;
        }
    }
    Ok(())
}

/// Normalize one writable singleton root member in place, if it declares a
/// `$normalize` (§8.2/§8.8): `.` is the member's own value, evaluated over the
/// package root. The singleton analogue of [`normalize_field`], reached from the
/// singleton write path and the seed path rather than a keyed collection.
pub(crate) fn normalize_singleton_field(
    compiled: &Compiled,
    name: &str,
    fields: &mut FieldMap,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<(), Rejection> {
    let Some(normalize) = compiled.singleton_normalize(name) else { return Ok(()) };
    let current = Cell::Scalar(fields.get(name).cloned().unwrap_or(Value::None));
    let value = scalar(ctx.eval(prospective, normalize, &current)?);
    fields.insert(name.to_owned(), value);
    Ok(())
}

/// Normalize one field in place, if it declares a `$normalize` (§8.8).
pub(crate) fn normalize_field(
    collection: &CompiledCollection,
    name: &str,
    fields: &mut FieldMap,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<(), Rejection> {
    let Some(field) = collection.field(name) else { return Ok(()) };
    let Some((typed, _)) = &field.normalize else { return Ok(()) };
    let current = Cell::Scalar(fields.get(name).cloned().unwrap_or(Value::None));
    let value = scalar(ctx.eval(prospective, typed, &current)?);
    fields.insert(name.to_owned(), value);
    Ok(())
}

/// Validate the final prospective state of every touched row (§8.8): field and
/// row checks, reference integrity, and additional uniqueness.
pub(crate) fn finalize(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    touched: &[RowAddress],
) -> Result<(), Rejection> {
    for address in touched {
        let Some(fields) = prospective.get(address) else { continue };
        // The declaration-name path of the touched row resolves its compiled
        // collection, top-level or nested (§5.4).
        let decl: Vec<String> = address.steps().map(|s| s.name().as_str().to_owned()).collect();
        let Some(name) = decl.last().cloned() else { continue };
        let Some(collection) = compiled.collection_at(&decl) else { continue };
        check_key_components(address)?;
        check_fields(collection, fields, address, ctx, prospective)?;
        // §5.2/§5.4/§5.5: a row or static-struct `$check` reads the COMPLETE
        // prospective row — plain fields, static structs, computed values folded in
        // dependency order, AND nested keyed collections (an omitted child collection
        // is empty, not absent) — so a check aggregating a nested child collection
        // (`count(.departments) >= 0`) resolves instead of faulting on a missing
        // member. `materialize_row_cell` is the ONE canonical complete builder (it
        // descends nested collections and folds computed values, degrading to the
        // field/struct row when there is neither); the `row_cell_of` fallback covers
        // only the impossible just-removed row. Built once and shared by both checks.
        if !collection.row_checks.is_empty() || !collection.structs.is_empty() {
            let row = ctx
                .materialize_row_cell(prospective, &decl, address)
                .unwrap_or_else(|| ctx.row_cell_of(prospective, collection, fields));
            check_row(collection, &row, address, ctx, prospective)?;
            check_structs(collection, fields, &row, address, ctx, prospective)?;
        }
        check_refs(compiled, prospective, collection, fields, address)?;
        check_uniqueness(prospective, collection, fields, address)?;
        if let Some(bucket) = compiled.bucket(&name) {
            crate::bucket::check_interval(bucket, collection, fields, ctx.now, &address.render())?;
        }
    }
    Ok(())
}

/// Reject a row whose key flattens to an empty canonical component — an empty
/// `text` or `bytes` (A.8/D.2, SPEC-ISSUES item 31). An empty component makes a
/// display path (D.3) non-injective / non-round-trippable, so it is inadmissible
/// in the same failure class as an unpopulated required key field (§22.1). Every
/// step is checked so no ancestor key segment is empty either; the
/// `liasse_ident` key builder is the single point that classifies emptiness.
fn check_key_components(address: &RowAddress) -> Result<(), Rejection> {
    for step in address.steps() {
        // §8.2/§23.3: the reserved `$root` singleton row carries a synthetic
        // empty-text key — an implementation-owned address, never an application
        // display-path segment (D.3) — so it is exempt from the non-empty rule.
        if step.name().as_str() == crate::singleton::ROOT_NAME {
            continue;
        }
        let components: Vec<Value> = step.key().components().cloned().collect();
        if matches!(
            liasse_ident::KeyText::from_key_values(&components),
            Err(liasse_ident::IdentError::EmptyKeyComponent)
        ) {
            return Err(Rejection::new(
                RejectionReason::Malformed,
                "a key component is the empty canonical value (empty text or bytes); an empty key \
                 component is not admissible (A.8/D.2)",
            )
            .at(address.render()));
        }
    }
    Ok(())
}

fn check_fields(
    collection: &CompiledCollection,
    fields: &FieldMap,
    address: &RowAddress,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<(), Rejection> {
    for field in &collection.fields {
        if field.checks.is_empty() {
            continue;
        }
        let current = Cell::Scalar(fields.get(&field.name).cloned().unwrap_or(Value::None));
        for check in &field.checks {
            if !passes(ctx.eval(prospective, &check.condition, &current)?) {
                return Err(Rejection::new(RejectionReason::Check, check.message.clone())
                    .at(format!("{}/{}", address.render(), field.name)));
            }
        }
    }
    Ok(())
}

/// Enforce every row `$check` (§8.8) over the COMPLETE prospective row `current`
/// (§5.2 computed values, §5.4 nested keyed collections, §5.5 sets) built once by
/// [`finalize`], so a `$check` reading `.label` (a computed) or aggregating
/// `.departments` (a nested child collection) enforces instead of faulting.
fn check_row(
    collection: &CompiledCollection,
    current: &Cell,
    address: &RowAddress,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<(), Rejection> {
    for check in &collection.row_checks {
        if !passes(ctx.eval(prospective, &check.condition, current)?) {
            return Err(Rejection::new(RejectionReason::Check, check.message.clone())
                .at(address.render()));
        }
    }
    Ok(())
}

/// Enforce every static-struct `$check` (§5.3/§5.10/§8.8) over the FINAL
/// prospective row, descending nested structs to any depth. A struct materializes
/// as a `Value::Struct` scalar; its check reads the struct as `.` and — through
/// `^`, `^^`, … — the containing row and any ancestor structs (§6.2), so the
/// lexical parent chain is supplied here rather than at struct-construction time,
/// where the containing row is not yet assembled. This mirrors the read/view path
/// (`fold_struct_computed`) fdc7639 established: without the parent frame a struct
/// check reading `^` overruns the evaluator's scope stack and faults on every
/// insert instead of enforcing.
fn check_structs(
    collection: &CompiledCollection,
    fields: &FieldMap,
    row: &Cell,
    address: &RowAddress,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<(), Rejection> {
    if collection.structs.is_empty() {
        return Ok(());
    }
    // The containing row supplied as a struct check's `^` frame (§6.2) is the
    // COMPLETE prospective row `finalize` built — fields, sibling static structs,
    // computed values (§5.2), and nested keyed collections (§5.4) — so a struct
    // `$check` reading `^.<computed>` or `^.<child_collection>` resolves like any
    // other ancestor member instead of faulting.
    for structure in &collection.structs {
        if let Some(value) = fields.get(&structure.name) {
            check_struct(structure, value, std::slice::from_ref(row), address, ctx, prospective)?;
        }
    }
    Ok(())
}

/// Enforce one static struct's `$check`s with `parents` (outermost-first) as its
/// lexical ancestor chain, then recurse into its nested structs (§5.3), extending
/// the chain so a deeper check reaches an ancestor field through `^`, `^^`, ….
fn check_struct(
    structure: &CompiledStruct,
    value: &Value,
    parents: &[Cell],
    address: &RowAddress,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<(), Rejection> {
    let struct_cell = Cell::Scalar(value.clone());
    if !structure.row_checks.is_empty() {
        let mut currents = parents.to_vec();
        currents.push(struct_cell.clone());
        for check in &structure.row_checks {
            if !passes(ctx.eval_scoped(prospective, &check.condition, &currents)?) {
                return Err(Rejection::new(RejectionReason::Check, check.message.clone()).at(address.render()));
            }
        }
    }
    if !structure.structs.is_empty()
        && let Value::Struct(inner) = value
    {
        let mut child_parents = parents.to_vec();
        child_parents.push(struct_cell);
        for nested in &structure.structs {
            if let Some(nested_value) = inner.get(&nested.name) {
                check_struct(nested, nested_value, &child_parents, address, ctx, prospective)?;
            }
        }
    }
    Ok(())
}

fn check_refs(
    compiled: &Compiled,
    prospective: &Prospective,
    collection: &CompiledCollection,
    fields: &FieldMap,
    address: &RowAddress,
) -> Result<(), Rejection> {
    // §5.3/§5.6/§22.1: a `$ref` is a legal static-struct member, so reference
    // validity must hold for a struct-nested ref exactly as for a top-level one.
    // Walk every reference-bearing field of the row's struct tree (recursively),
    // not just `collection.fields`, so a dangling struct-nested ref rejects.
    for site in crate::refwalk::ref_sites(collection) {
        let field = site.field;
        if let Some(info) = &field.reference {
            match site.value(fields) {
                None | Some(Value::None) if info.optional => {}
                None | Some(Value::None) => {
                    return Err(Rejection::new(
                        RejectionReason::DanglingRef,
                        format!("required reference `{}` has no target", site.display_name()),
                    )
                    .at(address.render()));
                }
                Some(Value::Ref(reference)) => {
                    if !target_present(compiled, prospective, &info.target, reference.key()) {
                        return Err(Rejection::new(
                            RejectionReason::DanglingRef,
                            format!("reference `{}` does not resolve to a live row", site.display_name()),
                        )
                        .at(address.render()));
                    }
                }
                Some(_) => {}
            }
        }
        // §5.5/§5.6: every member of a `$set` of `$ref` is a reference that MUST
        // resolve to a live row, exactly like a scalar ref field — a dangling
        // member rejects the whole transition (§22.1 reference validity).
        if let Some(info) = &field.element_reference
            && let Some(Value::Set(members)) = site.value(fields)
        {
            for member in members {
                let Some(key) = member_ref_key(member) else { continue };
                if !target_present(compiled, prospective, &info.target, &key) {
                    return Err(Rejection::new(
                        RejectionReason::DanglingRef,
                        format!(
                            "a member of reference set `{}` does not resolve to a live row",
                            site.display_name()
                        ),
                    )
                    .at(address.render()));
                }
            }
        }
    }
    Ok(())
}

/// The target key a `$set`-of-`$ref` member carries (§5.6): a `Ref` exposes its
/// key directly; a member stored as its bare scalar key (§6.3 ref/key equality)
/// is that single-component key. A `none` member carries no target.
fn member_ref_key(value: &Value) -> Option<RefKey> {
    match value {
        Value::Ref(reference) => Some(reference.key().clone()),
        Value::None => None,
        other => Some(RefKey::Scalar(Box::new(other.clone()))),
    }
}

/// Whether the target collection holds a live row whose key matches `key`.
///
/// §6.3/§7.6/A.9: a reference resolves by comparing its *application key* against
/// each target row's application-visible key identity (§5.4). A single-field key
/// compares as its bare scalar; a composite key compares as the name-sorted
/// struct a target row materializes (`materialize::key_identity`) — which is the
/// same shape a composite ref carries, since a ref to a composite target is typed
/// `RefTarget::Scalar(Struct)` and decodes to `Ref::scalar(Struct)`. Comparing
/// the two reconciled identities (rather than the old positional-vs-first-only
/// component test) lets a composite ref resolve to its target regardless of
/// whether it is carried as that name-sorted struct or a positional `$key`-order
/// tuple. A ref naming no declared collection resolves nowhere.
fn target_present(
    compiled: &Compiled,
    prospective: &Prospective,
    target: &str,
    key: &RefKey,
) -> bool {
    // §5.6/§A.9: a ref target is a `/`-separated declaration-name path — a
    // top-level collection (`companies`) or a nested one (`companies/offices`).
    match target.split('/').collect::<Vec<_>>().as_slice() {
        [name] => {
            let Some(collection) = compiled.collection(name) else { return false };
            let names = collection.key.as_slice();
            let wanted = ref_identity(names, key);
            let path = CollectionPath::top(NameSegment::new(*name));
            prospective.addresses_in(&path).iter().any(|address| {
                address.steps().last().is_some_and(|step| {
                    let components: Vec<Value> = step.key().components().cloned().collect();
                    identity_of(names, &components) == wanted
                })
            })
        }
        segments => {
            // §5.4/§D.1/§A.9: a ref to a NESTED collection carries the target row's
            // FULL identity — every ancestor `$key` followed by the local `$key`, in
            // ancestor-then-local order. Resolve the declaration PATH and compare the
            // ref's positional components against each candidate row's flattened
            // address key (ancestors-then-local), so a ref to a live nested row
            // resolves rather than being rejected as dangling.
            let decl: Vec<String> = segments.iter().map(|s| (*s).to_owned()).collect();
            if compiled.collection_at(&decl).is_none() {
                return false;
            }
            let wanted = ref_key_components(key);
            prospective.working().keys().any(|address| {
                address_declaration(address) == decl
                    && address_key_components(address) == wanted
            })
        }
    }
}

/// The positional key components a reference carries, in ancestor-then-local
/// order (§A.9): a scalar ref is one component, a composite ref its ordered
/// components. Used to match a ref against a nested target's flattened identity.
fn ref_key_components(key: &RefKey) -> Vec<Value> {
    match key {
        RefKey::Scalar(value) => vec![(**value).clone()],
        RefKey::Composite(components) => components.clone(),
    }
}

/// A row address's declaration-name path (the ordered collection names, root
/// first): `["companies", "offices"]` for `/companies/acme/offices/hq`.
fn address_declaration(address: &RowAddress) -> Vec<String> {
    address.steps().map(|step| step.name().as_str().to_owned()).collect()
}

/// A row's full identity components in ancestor-then-local order (§D.1): every
/// ancestor `$key` component followed by the local `$key` components, flattened.
fn address_key_components(address: &RowAddress) -> Vec<Value> {
    address.steps().flat_map(|step| step.key().components().cloned()).collect()
}

fn check_uniqueness(
    prospective: &Prospective,
    collection: &CompiledCollection,
    fields: &FieldMap,
    address: &RowAddress,
) -> Result<(), Rejection> {
    if collection.unique.is_empty() {
        return Ok(());
    }
    // §5.7: nested uniqueness is scoped to the parent row — the candidate set is
    // the siblings under this row's own collection path (ancestors included), so
    // the same value under a different parent does not conflict.
    let path = address.collection();
    let others: Vec<RowAddress> = prospective
        .addresses_in(&path)
        .into_iter()
        .filter(|other| other != address)
        .collect();
    for group in &collection.unique {
        let Some(tuple) = tuple_of(group, fields) else { continue };
        for other in &others {
            let Some(other_fields) = prospective.get(other) else { continue };
            if tuple_of(group, other_fields).is_some_and(|t| t == tuple) {
                return Err(Rejection::new(
                    RejectionReason::Uniqueness,
                    format!("uniqueness constraint on ({}) is violated", group.join(", ")),
                )
                .at(address.render()));
            }
        }
    }
    Ok(())
}

/// The candidate-key tuple of a row, or `None` if any component is absent
/// (an optional-none component does not conflict, §5.7).
fn tuple_of(group: &[String], fields: &FieldMap) -> Option<Vec<Value>> {
    group
        .iter()
        .map(|name| match fields.get(name) {
            Some(Value::None) | None => None,
            Some(value) => Some(value.clone()),
        })
        .collect()
}

fn scalar(cell: Cell) -> Value {
    match cell {
        Cell::Scalar(value) => value,
        _ => Value::None,
    }
}

fn passes(cell: Cell) -> bool {
    matches!(cell, Cell::Scalar(Value::Bool(true)))
}
