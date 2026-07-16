//! Load-time compilation: the once-per-definition work the admission hot path
//! reuses.
//!
//! The model proves the definition statically valid but keeps neither the typed
//! programs nor the seed. Rather than re-parse and re-type-check on every
//! request, the engine compiles — once, at load — each collection's defaults,
//! normalizers, and checks into typed expressions, each mutation's statement
//! program with its parameter scope, and each view into a typed expression. The
//! result is an owned [`Compiled`] the engine holds beside the model and store.

use liasse_diag::{SourceId, SourceMap};
use liasse_expr::{check_statement, ExprType, RowType, Scope, TypedExpr};
use liasse_model::{Collection, Model, Node};
use liasse_syntax::{parse_expression, Stmt};
use liasse_value::Type;

use crate::doc;
use crate::error::EngineError;
use crate::schema::Schema;
use crate::scope::RuntimeScope;

/// A compiled boolean check: its condition and diagnostic message (§8.8).
pub(crate) struct CompiledCheck {
    pub(crate) condition: TypedExpr,
    pub(crate) message: String,
}

/// A reference field's target (§5.6): the absolute target collection name and
/// whether the ref is optional.
pub(crate) struct RefInfo {
    pub(crate) target: String,
    pub(crate) optional: bool,
}

/// A compiled writable field of a collection row.
pub(crate) struct CompiledField {
    pub(crate) name: String,
    pub(crate) ty: Type,
    pub(crate) reference: Option<RefInfo>,
    pub(crate) default: Option<(TypedExpr, SourceId)>,
    pub(crate) normalize: Option<(TypedExpr, SourceId)>,
    pub(crate) checks: Vec<CompiledCheck>,
}

/// A compiled top-level keyed collection: its identity, fields, and constraints.
pub(crate) struct CompiledCollection {
    pub(crate) name: String,
    pub(crate) key: Vec<String>,
    pub(crate) unique: Vec<Vec<String>>,
    pub(crate) fields: Vec<CompiledField>,
    pub(crate) row_checks: Vec<CompiledCheck>,
}

impl CompiledCollection {
    /// The field descriptor named `name`, if declared.
    pub(crate) fn field(&self, name: &str) -> Option<&CompiledField> {
        self.fields.iter().find(|f| f.name == name)
    }
}

/// One statement of a mutation program with the sub-source its spans index.
pub(crate) struct CompiledStmt {
    pub(crate) stmt: Stmt,
    pub(crate) source: SourceId,
}

/// A compiled mutation program (§8).
pub(crate) struct CompiledMutation {
    pub(crate) name: String,
    pub(crate) path: Vec<String>,
    pub(crate) receiver_is_root: bool,
    pub(crate) params: Vec<(String, ExprType)>,
    pub(crate) scope: RuntimeScope,
    pub(crate) program: Vec<CompiledStmt>,
}

/// A compiled view (§7): its name and its typed expression.
pub(crate) struct CompiledView {
    pub(crate) name: String,
    pub(crate) expr: TypedExpr,
}

/// A compiled lifecycle bucket (§14): the collection it bounds and its optional
/// `$from`/`$until` interval expressions over the collection row. An absent
/// bound leaves that side of the interval unconstrained (`$from` = from
/// creation, `$until` = unbounded), per the §14.1 half-open interpretation.
pub(crate) struct CompiledBucket {
    pub(crate) collection: String,
    pub(crate) from: Option<TypedExpr>,
    pub(crate) until: Option<TypedExpr>,
}

/// The compiled artefacts the engine reuses across requests.
pub(crate) struct Compiled {
    pub(crate) collections: Vec<CompiledCollection>,
    pub(crate) mutations: Vec<CompiledMutation>,
    pub(crate) views: Vec<CompiledView>,
    pub(crate) buckets: Vec<CompiledBucket>,
}

impl Compiled {
    /// Compile a validated model against its source document.
    pub(crate) fn build(
        sources: &mut SourceMap,
        model: &Model,
        model_doc: &liasse_syntax::DocValue,
    ) -> Result<Self, EngineError> {
        let schema = Schema::new(model);
        let root_ty = ExprType::Row(schema.root_row_type());
        let collections = compile_collections(sources, schema, &root_ty)?;
        let mutations = compile_mutations(sources, schema, &root_ty, model_doc)?;
        let views = compile_views(sources, schema, &root_ty)?;
        let buckets = compile_buckets(sources, schema, &root_ty, model_doc)?;
        Ok(Self { collections, mutations, views, buckets })
    }

    /// The compiled collection named `name`, if any.
    pub(crate) fn collection(&self, name: &str) -> Option<&CompiledCollection> {
        self.collections.iter().find(|c| c.name == name)
    }

    /// The compiled mutation named `name`, if any.
    pub(crate) fn mutation(&self, name: &str) -> Option<&CompiledMutation> {
        self.mutations.iter().find(|m| m.name == name)
    }

    /// The compiled view named `name`, if any.
    pub(crate) fn view(&self, name: &str) -> Option<&CompiledView> {
        self.views.iter().find(|v| v.name == name)
    }

    /// The compiled bucket bounding collection `name`, if it is bucketed.
    pub(crate) fn bucket(&self, name: &str) -> Option<&CompiledBucket> {
        self.buckets.iter().find(|b| b.collection == name)
    }
}

fn compile_expr(
    sources: &mut SourceMap,
    scope: &dyn Scope,
    label: &str,
    text: &str,
) -> Result<(TypedExpr, SourceId), EngineError> {
    let src = sources.add_label(label, text.to_owned());
    let parsed = parse_expression(src, text).map_err(|d| EngineError::Invalid(Box::new(d)))?;
    let typed = check_statement(scope, src, &parsed).map_err(|d| EngineError::Invalid(Box::new(d)))?;
    Ok((typed, src))
}

fn compile_checks(
    sources: &mut SourceMap,
    scope: &dyn Scope,
    label: &str,
    checks: &[liasse_model::Check],
) -> Result<Vec<CompiledCheck>, EngineError> {
    let mut out = Vec::new();
    for check in checks {
        let (condition, _source) = compile_expr(sources, scope, label, &check.condition.text)?;
        let message = check
            .message
            .clone()
            .unwrap_or_else(|| format!("check failed: {}", check.condition.text));
        out.push(CompiledCheck { condition, message });
    }
    Ok(out)
}

fn compile_collections(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
) -> Result<Vec<CompiledCollection>, EngineError> {
    let mut out = Vec::new();
    for member in &schema.model().root().members {
        if let Node::Collection(collection) = &member.node {
            out.push(compile_collection(sources, schema, root_ty, member.name.as_str(), collection)?);
        }
    }
    Ok(out)
}

fn compile_collection(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    name: &str,
    collection: &Collection,
) -> Result<CompiledCollection, EngineError> {
    let row_ty = schema
        .receiver_row_type(std::slice::from_ref(&name.to_owned()))
        .unwrap_or_else(|| ExprType::Row(RowType::keyless(std::iter::empty())));
    let row_scope = RuntimeScope::new(row_ty.clone(), root_ty.clone());

    let mut fields = Vec::new();
    let mut unique: Vec<Vec<String>> = collection
        .unique
        .iter()
        .map(|group| group.iter().map(|f| f.as_str().to_owned()).collect())
        .collect();

    for member in &collection.shape.members {
        let field = match &member.node {
            // A read-only computed value inside a row is not an insertable or
            // assignable field (§5.2); it is a documented CORE seam.
            Node::Scalar(scalar) if !scalar.is_writable() => continue,
            Node::Scalar(scalar) => {
                if scalar.unique {
                    unique.push(vec![member.name.as_str().to_owned()]);
                }
                let default = match &scalar.default {
                    Some(source) => Some(compile_expr(sources, &row_scope, "default", &source.text)?),
                    None => None,
                };
                let field_scope = RuntimeScope::new(ExprType::scalar(scalar.ty.clone()), root_ty.clone());
                let normalize = match &scalar.normalize {
                    Some(source) => Some(compile_expr(sources, &field_scope, "normalize", &source.text)?),
                    None => None,
                };
                let checks = compile_checks(sources, &field_scope, "check", &scalar.checks)?;
                CompiledField {
                    name: member.name.as_str().to_owned(),
                    ty: scalar.ty.clone(),
                    reference: None,
                    default,
                    normalize,
                    checks,
                }
            }
            Node::Reference(reference) => CompiledField {
                name: member.name.as_str().to_owned(),
                ty: Type::Ref(liasse_value::RefTarget::Scalar(Box::new(reference.key_type.clone()))),
                reference: Some(RefInfo {
                    target: reference.target.trim_start_matches('/').to_owned(),
                    optional: reference.optional,
                }),
                default: None,
                normalize: None,
                checks: Vec::new(),
            },
            Node::Set(set) => CompiledField {
                name: member.name.as_str().to_owned(),
                ty: Type::Set(Box::new(set.element.clone())),
                reference: None,
                default: None,
                normalize: None,
                checks: Vec::new(),
            },
            // Nested structs/collections, computed values, and views inside a row
            // are documented CORE seams (not insertable/patchable fields here).
            _ => continue,
        };
        fields.push(field);
    }

    let row_checks = compile_checks(sources, &row_scope, "row-check", &collection.shape.checks)?;

    Ok(CompiledCollection {
        name: name.to_owned(),
        key: collection.key.iter().map(|f| f.as_str().to_owned()).collect(),
        unique,
        fields,
        row_checks,
    })
}

fn compile_mutations(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
) -> Result<Vec<CompiledMutation>, EngineError> {
    let mut out = Vec::new();
    for mutation in schema.model().mutations() {
        let receiver_ty = schema
            .receiver_row_type(&mutation.path)
            .ok_or_else(|| EngineError::Internal(format!("mutation `{}` has no receiver", mutation.name.as_str())))?;
        let mut scope = RuntimeScope::new(receiver_ty, root_ty.clone());
        for (name, ty) in &mutation.params {
            scope = scope.with_param(name.clone(), ty.clone());
        }
        let bodies = mutation_bodies(model_doc, mutation)?;
        let mut program = Vec::new();
        for text in bodies {
            let src = sources.add_label("mut", text.clone());
            let parsed = parse_expression(src, &text).map_err(|d| EngineError::Invalid(Box::new(d)))?;
            program.push(CompiledStmt { stmt: parsed.statement, source: src });
        }
        out.push(CompiledMutation {
            name: mutation.name.as_str().to_owned(),
            path: mutation.path.clone(),
            receiver_is_root: mutation.path.is_empty(),
            params: mutation.params.clone(),
            scope,
            program,
        });
    }
    Ok(out)
}

/// The statement-string bodies of a mutation, read from the `$mut` member of its
/// receiver shape in the document (§8.1).
fn mutation_bodies(
    model_doc: &liasse_syntax::DocValue,
    mutation: &liasse_model::Mutation,
) -> Result<Vec<String>, EngineError> {
    let shape = doc::shape_at(model_doc, &mutation.path)
        .ok_or_else(|| EngineError::Internal("mutation receiver shape not found".to_owned()))?;
    let muts = doc::member(shape, "$mut")
        .ok_or_else(|| EngineError::Internal("mutation receiver has no `$mut`".to_owned()))?;
    let members = doc::object(muts)
        .ok_or_else(|| EngineError::Internal("`$mut` is not an object".to_owned()))?;
    let body = members
        .iter()
        .find(|m| mut_base_name(&m.name.text) == mutation.name.as_str())
        .map(|m| &m.value)
        .ok_or_else(|| EngineError::Internal(format!("mutation `{}` body missing", mutation.name.as_str())))?;
    if let Some(text) = doc::string(body) {
        Ok(vec![text.to_owned()])
    } else if let Some(items) = doc::array(body) {
        Ok(items.iter().filter_map(doc::string).map(str::to_owned).collect())
    } else {
        Err(EngineError::Internal("mutation body is not a string or array".to_owned()))
    }
}

/// The base name of a `$mut` member key, dropping any `name({ proto })`
/// prototype suffix (§8.3).
fn mut_base_name(key: &str) -> &str {
    key.split('(').next().unwrap_or(key).trim()
}

fn compile_views(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
) -> Result<Vec<CompiledView>, EngineError> {
    let scope = RuntimeScope::new(root_ty.clone(), root_ty.clone());
    let mut out = Vec::new();
    for member in &schema.model().root().members {
        if let Node::View(view) = &member.node {
            let (expr, _source) = compile_expr(sources, &scope, "view", &view.expr.text)?;
            out.push(CompiledView { name: member.name.as_str().to_owned(), expr });
        }
    }
    Ok(out)
}

/// Compile each top-level keyed collection's `$bucket` interval expressions
/// (§14). The declaration is read straight from the document because the model
/// validates but does not retain it. Source-backed and recurring buckets
/// (`$source`/`$repeat`) are skipped as documented CORE seams, leaving the
/// collection with ordinary, always-active read semantics.
fn compile_buckets(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
) -> Result<Vec<CompiledBucket>, EngineError> {
    let mut out = Vec::new();
    for member in &schema.model().root().members {
        if !matches!(&member.node, Node::Collection(_)) {
            continue;
        }
        let name = member.name.as_str().to_owned();
        let Some(shape) = doc::shape_at(model_doc, std::slice::from_ref(&name)) else {
            continue;
        };
        let Some(bucket_doc) = doc::member(shape, "$bucket") else {
            continue;
        };
        let row_ty = schema
            .receiver_row_type(std::slice::from_ref(&name))
            .unwrap_or_else(|| ExprType::Row(RowType::keyless(std::iter::empty())));
        let scope = RuntimeScope::new(row_ty, root_ty.clone());
        let Some(bucket) = compile_bucket(sources, &scope, &name, bucket_doc)? else {
            continue;
        };
        out.push(bucket);
    }
    Ok(out)
}

/// Compile one `$bucket` declaration into its interval expressions, or `None`
/// when it is a source-backed/recurring form left as a CORE seam.
fn compile_bucket(
    sources: &mut SourceMap,
    scope: &RuntimeScope,
    name: &str,
    bucket_doc: &liasse_syntax::DocValue,
) -> Result<Option<CompiledBucket>, EngineError> {
    // Short form: a lone until-expression; `$from` defaults to row creation.
    if let Some(text) = doc::string(bucket_doc) {
        let (until, _) = compile_expr(sources, scope, "bucket", text)?;
        return Ok(Some(CompiledBucket { collection: name.to_owned(), from: None, until: Some(until) }));
    }
    let Some(members) = doc::object(bucket_doc) else {
        return Ok(None);
    };
    // Source-backed and recurring buckets are documented seams.
    if members.iter().any(|m| m.name.text == "$source" || m.name.text == "$repeat") {
        return Ok(None);
    }
    let mut from = None;
    let mut until = None;
    for member in members {
        let Some(text) = doc::string(&member.value) else { continue };
        match member.name.text.as_str() {
            // `$from: $created` is the "from creation" default; not stored, so it
            // needs no bound expression (see the bucket module's CORE note).
            "$from" if text.trim() != "$created" => from = Some(compile_expr(sources, scope, "bucket", text)?.0),
            "$until" => until = Some(compile_expr(sources, scope, "bucket", text)?.0),
            _ => {}
        }
    }
    Ok(Some(CompiledBucket { collection: name.to_owned(), from, until }))
}
