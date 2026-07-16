//! Meters (SPEC.md §15): capacity pools, spend allocation, and the §15.6
//! accessors.
//!
//! The model validates `$limits`/`$consumes`/`$sources` syntactically but keeps
//! no structured declaration, so — like [`compile_buckets`](crate::compiled) —
//! this module reads the meter declarations from the definition document and
//! compiles them once (`$sources` pool views, `$eligible`, `$order`, and each
//! `$consumes` amount/time/metadata expression).
//!
//! At admission [`admit::enforce`] funds every new or changed spend: it resolves
//! the pools reachable from the spend's ancestor chain active at the spend's
//! `$time` (§15.1 temporal context), coalesces duplicate pool identities, gates
//! them by `$eligible`, drains capacity in `$order`, and rejects the whole
//! transition when eligible capacity is insufficient (§15.2). The chosen
//! allocation is frozen onto the spend row as an admission fact
//! ([`FUNDING_FIELD`]) so a later pool change never rewrites it (§15.2/§15.3);
//! deleting the spend releases it, updating it releases and reallocates.
//!
//! [`accessor`] materializes the read-facing cells the §15.6 accessors expose:
//! `.<meter>.balance` (the non-negative remainder of pool capacity after extant
//! allocations), `.<meter>.pools`, and `spend.funding`.

use liasse_diag::SourceMap;
use liasse_expr::{ExprType, RowType, TypedExpr};
use liasse_value::Type;

use crate::compiled::compile_expr as compile_expr_in;
use crate::error::EngineError;
use crate::schema::Schema;
use crate::scope::RuntimeScope;

pub(crate) mod accessor;
pub(crate) mod admit;
mod resolve;

/// The structural field a spend row carries its frozen funding allocation in
/// (§15.3). Not a declared shape member, so it is invisible to ordinary reads;
/// the `funding` accessor projects it and the balance accessor sums it.
pub(crate) const FUNDING_FIELD: &str = "$funding";

/// One `$sources` pool view of a meter (§15.1): the stable label that becomes
/// part of funding identity and the typed pool view evaluated over the enforcing
/// row. `has_quantity` records whether the view projects a `$quantity` capacity;
/// a source without one partitions rather than limits (§14.8).
pub(crate) struct CompiledSource {
    pub(crate) label: String,
    pub(crate) view: TypedExpr,
    pub(crate) has_quantity: bool,
}

/// One `$order` comparison key (§15.2), highest priority first.
pub(crate) struct OrderKey {
    pub(crate) expr: TypedExpr,
    pub(crate) descending: bool,
}

/// A compiled meter declaration at one enforcing collection level (§15.1).
pub(crate) struct CompiledMeter {
    /// The declaration-name path of the enforcing collection (`["accounts"]`,
    /// `["companies", "accounts"]`).
    pub(crate) path: Vec<String>,
    /// The meter name.
    pub(crate) name: String,
    pub(crate) sources: Vec<CompiledSource>,
    /// `$eligible`, typed over `pool` and `spend` bindings (§15.2).
    pub(crate) eligible: Option<TypedExpr>,
    pub(crate) order: Vec<OrderKey>,
    /// Whether any source declares `$quantity`: a limiting meter drains capacity,
    /// a purely partitioning one never rejects for capacity (§14.8, §15.1).
    pub(crate) limiting: bool,
}

impl CompiledMeter {
    /// A stable identity for this enforcing level's allocations against a concrete
    /// enforcing row, used to tag and match funding entries (§15.4 keeps each
    /// ancestor level's allocations distinct).
    pub(crate) fn level_id(&self, enforcing: &liasse_store::RowAddress) -> String {
        format!("{}\u{0}{}", enforcing.render(), self.name)
    }
}

/// One meter a spend collection consumes (§15.1), with its amount/time/metadata
/// expressions over the spend row.
pub(crate) struct SpendConsume {
    pub(crate) meter: String,
    pub(crate) amount: TypedExpr,
    pub(crate) time: TypedExpr,
    pub(crate) metadata: Vec<(String, TypedExpr)>,
}

/// A compiled `$consumes` spend collection (§15.1).
pub(crate) struct CompiledSpend {
    pub(crate) path: Vec<String>,
    pub(crate) consumes: Vec<SpendConsume>,
}

/// The compiled meter artefacts the engine reuses across requests.
pub(crate) struct CompiledMeters {
    pub(crate) meters: Vec<CompiledMeter>,
    pub(crate) spends: Vec<CompiledSpend>,
}

impl CompiledMeters {
    /// The compiled spend collection at declaration path `path`, if it consumes.
    pub(crate) fn spend_at(&self, path: &[String]) -> Option<&CompiledSpend> {
        self.spends.iter().find(|s| s.path == path)
    }

    /// The meters declared at declaration path `path` named `name` — one per
    /// enforcing level, so a hierarchical lookup collects the whole ancestor
    /// chain (§15.4).
    pub(crate) fn meter_at<'a>(&'a self, path: &[String], name: &str) -> Option<&'a CompiledMeter> {
        self.meters.iter().find(|m| m.path == path && m.name == name)
    }
}

/// Compile every `$limits`/`$consumes` declaration in the model (§15.1), reading
/// the structured forms from `model_doc`.
pub(crate) fn compile(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
) -> Result<CompiledMeters, EngineError> {
    let mut out = CompiledMeters { meters: Vec::new(), spends: Vec::new() };
    for member in &schema.model().root().members {
        if let liasse_model::Node::Collection(collection) = &member.node {
            let path = vec![member.name.as_str().to_owned()];
            compile_at(sources, schema, root_ty, model_doc, &path, collection, &mut out)?;
        }
    }
    Ok(out)
}

fn compile_at(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
    path: &[String],
    collection: &liasse_model::Collection,
    out: &mut CompiledMeters,
) -> Result<(), EngineError> {
    let Some(shape) = crate::doc::shape_at(model_doc, path) else {
        return descend(sources, schema, root_ty, model_doc, path, collection, out);
    };
    if !collection.shape.meters.is_empty()
        && let Some(limits) = crate::doc::member(shape, "$limits")
    {
        compile_limits(sources, schema, root_ty, path, limits, out);
    }
    // §15: a `$consumes` whose amount/time/metadata does not re-type in the
    // runtime scope (e.g. a recurring source-backed pool §14.5 this stage does
    // not build) is a documented seam — the spend admits unmetered rather than
    // failing the whole package load.
    if collection.consumes
        && let Some(consumes) = crate::doc::member(shape, "$consumes")
        && let Ok(spend) = compile_consumes(sources, schema, root_ty, path, consumes)
    {
        out.spends.push(spend);
    }
    descend(sources, schema, root_ty, model_doc, path, collection, out)
}

fn descend(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
    path: &[String],
    collection: &liasse_model::Collection,
    out: &mut CompiledMeters,
) -> Result<(), EngineError> {
    for member in &collection.shape.members {
        if let liasse_model::Node::Collection(nested) = &member.node {
            let mut child = path.to_vec();
            child.push(member.name.as_str().to_owned());
            compile_at(sources, schema, root_ty, model_doc, &child, nested, out)?;
        }
    }
    Ok(())
}

fn compile_limits(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    path: &[String],
    limits: &liasse_syntax::DocValue,
    out: &mut CompiledMeters,
) {
    let Some(meters) = crate::doc::object(limits) else { return };
    let enforcing_ty = schema
        .receiver_row_type(path)
        .unwrap_or_else(|| ExprType::Row(RowType::keyless(std::iter::empty())));
    // §15.1: a meter source is evaluated in the temporal context of the spend, so a
    // recurring source-backed pool (§14.5) is read at a bounded instant, never
    // enumerated whole. Clear the unbounded marker on each source-bucket view in the
    // root type used to type meter sources so a bare bucketed pool source type-checks
    // (the enumeration guard applies to whole reads, not spend-time pool resolution).
    let bounded_root = bounded_root(root_ty);
    for meter in meters {
        let name = meter.name.text.clone();
        // A meter whose pool source / `$eligible` / `$order` does not re-type in
        // the runtime scope is a documented seam: skip it rather than fail the load.
        if let Ok(compiled) =
            compile_meter(sources, schema, &bounded_root, path, &enforcing_ty, &name, &meter.value)
        {
            out.meters.push(compiled);
        }
    }
}

/// A copy of the package-root row type with the unbounded-recurring marker cleared
/// on every source-backed bucket view (§14.5). Meter source expressions read a
/// bucketed pool at the spend instant (§15.1), a bounded read, so the enumeration
/// guard must not reject them.
fn bounded_root(root_ty: &ExprType) -> ExprType {
    let ExprType::Row(root) = root_ty else { return root_ty.clone() };
    let fields = root.fields().map(|(name, ty)| {
        let ty = match ty {
            ExprType::View(row) if row.is_unbounded() => ExprType::View(row.clone().unbounded(false)),
            other => other.clone(),
        };
        (name.clone(), ty)
    });
    ExprType::Row(RowType::new(fields, root.key().cloned()))
}

fn compile_meter(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    path: &[String],
    enforcing_ty: &ExprType,
    name: &str,
    body: &liasse_syntax::DocValue,
) -> Result<CompiledMeter, EngineError> {
    let Some(members) = crate::doc::object(body) else {
        return Err(EngineError::Internal(format!("meter `{name}` is not an object")));
    };
    let enforcing_scope = RuntimeScope::new(enforcing_ty.clone(), root_ty.clone());
    let mut sources_out = Vec::new();
    let mut order_doc = None;
    let mut eligible_doc = None;
    for member in members {
        match member.name.text.as_str() {
            "$sources" => {
                for source in crate::doc::object(&member.value).into_iter().flatten() {
                    let Some(text) = crate::doc::string(&source.value) else { continue };
                    let (view, _) = compile_expr_in(sources, &enforcing_scope, "meter-source", text)?;
                    let has_quantity = view
                        .ty()
                        .as_view()
                        .or_else(|| view.ty().as_row())
                        .is_some_and(|row| row.field("$quantity").is_some());
                    sources_out.push(CompiledSource { label: source.name.text.clone(), view, has_quantity });
                }
            }
            "$order" => order_doc = Some(&member.value),
            "$eligible" => eligible_doc = crate::doc::string(&member.value),
            _ => {}
        }
    }
    let pool_ty = pool_row_type(&sources_out);
    let limiting = sources_out.iter().any(|s| s.has_quantity);

    let eligible = match eligible_doc {
        Some(text) => {
            let scope = RuntimeScope::new(enforcing_ty.clone(), root_ty.clone())
                .with_binding("pool", ExprType::Row(pool_ty.clone()))
                .with_binding("spend", ExprType::Row(spend_binding_type(schema, path)));
            Some(compile_expr_in(sources, &scope, "meter-eligible", text)?.0)
        }
        None => None,
    };

    let mut order = Vec::new();
    if let Some(order_doc) = order_doc {
        let pool_scope = RuntimeScope::new(ExprType::Row(pool_ty.clone()), root_ty.clone())
            .with_structural("until", ExprType::scalar(Type::Optional(Box::new(Type::Timestamp(liasse_value::Precision::Seconds)))))
            .with_structural("from", ExprType::scalar(Type::Optional(Box::new(Type::Timestamp(liasse_value::Precision::Seconds)))))
            .with_structural("quantity", ExprType::scalar(Type::Decimal));
        for item in crate::doc::array(order_doc).into_iter().flatten() {
            let Some(text) = crate::doc::string(item) else { continue };
            let descending = text.trim_start().starts_with('-');
            let expr_text = text.trim_start().trim_start_matches('-');
            order.push(OrderKey {
                expr: compile_expr_in(sources, &pool_scope, "meter-order", expr_text)?.0,
                descending,
            });
        }
    }

    Ok(CompiledMeter {
        path: path.to_vec(),
        name: name.to_owned(),
        sources: sources_out,
        eligible,
        order,
        limiting,
    })
}

/// The `spend` binding row type for `$eligible` (§15.2): the union of the scalar
/// fields of every descendant spend collection that consumes a meter declared at
/// `path`, so `spend.<metadata>` type-checks. The concrete metadata values are
/// folded in as cells at eval time.
fn spend_binding_type(schema: Schema<'_>, path: &[String]) -> RowType {
    let mut fields: Vec<(String, ExprType)> = Vec::new();
    if let Some(collection) = schema.collection_at_path(path) {
        collect_spend_fields(collection, &mut fields);
    }
    RowType::keyless(fields)
}

/// Union the scalar fields of every descendant `$consumes` collection into
/// `fields` (a spend-binding field set for `$eligible`).
fn collect_spend_fields(collection: &liasse_model::Collection, fields: &mut Vec<(String, ExprType)>) {
    if collection.consumes {
        for member in &collection.shape.members {
            if let liasse_model::Node::Scalar(scalar) = &member.node {
                let name = member.name.as_str().to_owned();
                if fields.iter().all(|(n, _)| *n != name) {
                    fields.push((name, ExprType::scalar(scalar.ty.clone())));
                }
            }
        }
    }
    for member in &collection.shape.members {
        if let liasse_model::Node::Collection(nested) = &member.node {
            collect_spend_fields(nested, fields);
        }
    }
}

/// The merged pool row type across a meter's sources (§15.2): the union of every
/// projected metadata field plus the structural `$quantity`/`$from`/`$until`
/// capacity and interval cells a `pool` binding or `$order` key reads.
fn pool_row_type(sources: &[CompiledSource]) -> RowType {
    let mut fields: Vec<(String, ExprType)> = Vec::new();
    for source in sources {
        if let Some(row) = source.view.ty().as_view().or_else(|| source.view.ty().as_row()) {
            for (name, ty) in row.fields() {
                if fields.iter().all(|(n, _)| n != name) {
                    fields.push((name.clone(), ty.clone()));
                }
            }
        }
    }
    for (name, ty) in [
        ("$quantity", ExprType::scalar(Type::Decimal)),
        ("$from", ExprType::scalar(Type::Optional(Box::new(Type::Timestamp(liasse_value::Precision::Seconds))))),
        ("$until", ExprType::scalar(Type::Optional(Box::new(Type::Timestamp(liasse_value::Precision::Seconds))))),
    ] {
        if fields.iter().all(|(n, _)| n != name) {
            fields.push((name.to_owned(), ty));
        }
    }
    RowType::keyless(fields)
}

fn compile_consumes(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    path: &[String],
    consumes: &liasse_syntax::DocValue,
) -> Result<CompiledSpend, EngineError> {
    let spend_ty = schema
        .receiver_row_type(path)
        .unwrap_or_else(|| ExprType::Row(RowType::keyless(std::iter::empty())));
    let scope = RuntimeScope::new(spend_ty, root_ty.clone());

    let mut out = Vec::new();
    if let Some(name) = crate::doc::string(consumes) {
        out.push(SpendConsume {
            meter: name.trim().to_owned(),
            amount: compile_expr_in(sources, &scope, "spend-amount", ".amount")?.0,
            time: compile_expr_in(sources, &scope, "spend-time", ".occurred_at")?.0,
            metadata: Vec::new(),
        });
        return Ok(CompiledSpend { path: path.to_vec(), consumes: out });
    }
    for member in crate::doc::object(consumes).into_iter().flatten() {
        let meter = member.name.text.clone();
        // §15.1: `$amount`/`$time` default to `.amount`/`.occurred_at`, but a
        // config that overrides `$amount` (or a bare amount expression) means the
        // spend collection need not carry an `amount` field — so the default is
        // compiled only when it is actually the amount source.
        let mut amount_text: Option<String> = None;
        let mut time_text: Option<String> = None;
        let mut metadata = Vec::new();
        if let Some(text) = crate::doc::string(&member.value) {
            amount_text = Some(text.to_owned());
        } else if let Some(config) = crate::doc::object(&member.value) {
            for field in config {
                let Some(text) = crate::doc::string(&field.value) else { continue };
                match field.name.text.as_str() {
                    "$amount" => amount_text = Some(text.to_owned()),
                    "$time" => time_text = Some(text.to_owned()),
                    other => metadata.push((other.to_owned(), compile_expr_in(sources, &scope, "spend-meta", text)?.0)),
                }
            }
        }
        let amount = compile_expr_in(sources, &scope, "spend-amount", amount_text.as_deref().unwrap_or(".amount"))?.0;
        let time = compile_expr_in(sources, &scope, "spend-time", time_text.as_deref().unwrap_or(".occurred_at"))?.0;
        out.push(SpendConsume { meter, amount, time, metadata });
    }
    Ok(CompiledSpend { path: path.to_vec(), consumes: out })
}
