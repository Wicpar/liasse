//! The admission rule pipeline (§5, §8.8): insertion defaults by declaration,
//! normalization, and — over the final prospective state — field/row checks,
//! reference integrity, and additional uniqueness.
//!
//! Defaults resolve in declaration order (a documented CORE simplification of
//! the full dependency ordering §5.1 permits any topological evaluation of).
//! Every rejection leaves the prospective state to be discarded whole, so the
//! prior committed state is never touched.

use liasse_expr::Cell;
use liasse_ident::NameSegment;
use liasse_store::{CollectionPath, RowAddress};
use liasse_value::{RefKey, Value};

use crate::compiled::{Compiled, CompiledCollection};
use crate::error::{Rejection, RejectionReason};
use crate::eval::{row_cell, EvalCtx};
use crate::materialize::FieldMap;
use crate::state::Prospective;

/// Resolve insertion defaults for the omitted fields of a new row (§5.1), then
/// fill any still-absent declared field with `none`, so the row is complete.
pub(crate) fn apply_defaults(
    collection: &CompiledCollection,
    fields: &mut FieldMap,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<(), Rejection> {
    for field in &collection.fields {
        if fields.contains_key(&field.name) {
            continue;
        }
        if let Some((typed, _)) = &field.default {
            let current = row_cell(collection, fields);
            let value = scalar(ctx.eval(prospective, typed, &current)?);
            fields.insert(field.name.clone(), value);
        }
    }
    for field in &collection.fields {
        fields.entry(field.name.clone()).or_insert_with(|| absent_value(&field.ty));
    }
    Ok(())
}

/// The value of a declared field that was neither supplied nor defaulted (§5.1,
/// §5.5): an omitted `$set` starts empty, every other omitted field reads
/// `none`. A distinct empty set (not `none`) is what makes an omitted child set
/// project as `[]` and a later `+`/`-` union against the existing membership.
fn absent_value(ty: &liasse_value::Type) -> Value {
    match ty {
        liasse_value::Type::Set(_) => Value::Set(std::collections::BTreeSet::new()),
        _ => Value::None,
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
        let Some(name) = address.steps().last().map(|s| s.name().as_str().to_owned()) else {
            continue;
        };
        let Some(collection) = compiled.collection(&name) else { continue };
        check_fields(collection, fields, address, ctx, prospective)?;
        check_row(collection, fields, address, ctx, prospective)?;
        check_refs(prospective, collection, fields, address)?;
        check_uniqueness(prospective, collection, fields, address)?;
        if let Some(bucket) = compiled.bucket(&name) {
            crate::bucket::check_interval(bucket, collection, fields, ctx.now, &address.render())?;
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

fn check_row(
    collection: &CompiledCollection,
    fields: &FieldMap,
    address: &RowAddress,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<(), Rejection> {
    if collection.row_checks.is_empty() {
        return Ok(());
    }
    let current = row_cell(collection, fields);
    for check in &collection.row_checks {
        if !passes(ctx.eval(prospective, &check.condition, &current)?) {
            return Err(Rejection::new(RejectionReason::Check, check.message.clone())
                .at(address.render()));
        }
    }
    Ok(())
}

fn check_refs(
    prospective: &Prospective,
    collection: &CompiledCollection,
    fields: &FieldMap,
    address: &RowAddress,
) -> Result<(), Rejection> {
    for field in &collection.fields {
        let Some(info) = &field.reference else { continue };
        match fields.get(&field.name) {
            None | Some(Value::None) if info.optional => {}
            None | Some(Value::None) => {
                return Err(Rejection::new(
                    RejectionReason::DanglingRef,
                    format!("required reference `{}` has no target", field.name),
                )
                .at(address.render()));
            }
            Some(Value::Ref(reference)) => {
                if !target_present(prospective, &info.target, reference.key()) {
                    return Err(Rejection::new(
                        RejectionReason::DanglingRef,
                        format!("reference `{}` does not resolve to a live row", field.name),
                    )
                    .at(address.render()));
                }
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Whether the target collection holds a live row whose key matches `key`.
fn target_present(prospective: &Prospective, target: &str, key: &RefKey) -> bool {
    let path = CollectionPath::top(NameSegment::new(target));
    prospective.addresses_in(&path).iter().any(|address| {
        address.steps().last().is_some_and(|step| match key {
            RefKey::Scalar(value) => {
                let mut components = step.key().components();
                components.next() == Some(value) && components.next().is_none()
            }
            RefKey::Composite(values) => step.key().components().eq(values.iter()),
        })
    })
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
    let path = CollectionPath::top(NameSegment::new(collection.name.as_str()));
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
