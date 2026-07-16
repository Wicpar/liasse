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
use liasse_model::{Collection, Model, Node, Shape};
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

/// A reference field's target (§5.6): the absolute target collection name,
/// whether the ref is optional, and the `$on_delete` policy that governs the
/// referencing row when its target is deleted (§21.1).
pub(crate) struct RefInfo {
    pub(crate) target: String,
    pub(crate) optional: bool,
    pub(crate) on_delete: OnDelete,
}

/// A compiled `$on_delete` policy on an inbound reference (§21.1, §5.6). `Patch`
/// carries the typed patch object, evaluated per referencing row at delete time
/// with `.` = the referencing row and `$target` = the deleted target row.
pub(crate) enum OnDelete {
    /// No policy declared — the model proved the target is not deletable, so this
    /// edge is never exercised by a deletion.
    Undecided,
    /// Block the deletion while the referencing row survives.
    Restrict,
    /// Delete the referencing row too.
    Cascade,
    /// Clear this optional ref (`$on_delete: none`).
    Clear,
    /// Apply a `= { … }` patch to the surviving referencing row. Boxed so the
    /// large typed patch does not widen every `OnDelete` (clippy large-variant).
    Patch(Box<TypedExpr>),
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

/// A compiled read-only computed value of a collection row (§5.2): its name and
/// the typed expression that derives it from the row (`.` = the row). It is not
/// a writable field; the runtime evaluates it when materializing the row so a
/// view, projection, or `return` reads it like any stored value.
pub(crate) struct CompiledComputed {
    pub(crate) name: String,
    pub(crate) expr: TypedExpr,
}

/// A compiled keyed collection: its identity, fields, constraints, and — for a
/// nested collection (§5.4) — the child collections declared under its rows. A
/// top-level collection is the root of one such tree; `children` holds the
/// collections nested one level deeper, each compiled the same way.
pub(crate) struct CompiledCollection {
    pub(crate) name: String,
    pub(crate) key: Vec<String>,
    pub(crate) unique: Vec<Vec<String>>,
    pub(crate) fields: Vec<CompiledField>,
    pub(crate) computed: Vec<CompiledComputed>,
    pub(crate) row_checks: Vec<CompiledCheck>,
    /// Static struct members (§5.3): a plain nested object whose fields resolve
    /// their own defaults/normalizers during the containing insertion.
    pub(crate) structs: Vec<CompiledStruct>,
    /// Keyed collections nested directly under this collection's rows (§5.4).
    pub(crate) children: Vec<CompiledCollection>,
}

/// A compiled static struct member (§5.3): a plain nested object sharing the
/// containing row's identity and lifecycle. Its fields carry their own defaults
/// and normalizers, resolved during the containing insertion (§5.1).
pub(crate) struct CompiledStruct {
    pub(crate) name: String,
    pub(crate) fields: Vec<CompiledField>,
    pub(crate) row_checks: Vec<CompiledCheck>,
}

impl CompiledCollection {
    /// The field descriptor named `name`, if declared.
    pub(crate) fn field(&self, name: &str) -> Option<&CompiledField> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// The nested child collection named `name`, if declared under this row.
    pub(crate) fn child(&self, name: &str) -> Option<&CompiledCollection> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Descend a declaration-name path from this collection to a nested one. An
    /// empty tail is this collection; each further segment names a child.
    pub(crate) fn at<'a>(&'a self, path: &[String]) -> Option<&'a CompiledCollection> {
        match path.split_first() {
            None => Some(self),
            Some((head, rest)) => self.child(head)?.at(rest),
        }
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
    /// The `$actor`/`$session` structural bindings in scope for this program when
    /// admitted through an authenticated role (§11.1, §6.2), each a `(name, row
    /// type)` pair. Empty when the package declares no `$auth`. Carried alongside
    /// `scope` so a rebuilt patch scope re-registers them.
    pub(crate) context_structurals: Vec<(String, ExprType)>,
}

/// A compiled view (§7): its name and its typed expression.
pub(crate) struct CompiledView {
    pub(crate) name: String,
    pub(crate) expr: TypedExpr,
}

/// One declared `$params` entry of a surface view (§10.1): its name, its
/// contract type, and — when declared with a `= …` default — the typed default
/// expression a read binds when the caller omits the argument (§8.3).
pub(crate) struct CompiledParam {
    pub(crate) name: String,
    pub(crate) ty: ExprType,
    pub(crate) default: Option<TypedExpr>,
}

/// A compiled surface `$view` (§10.1): a `$public` or role `$view` whose scope
/// carries the surface `$params` (read as `@name`) and the request-scoped
/// `$actor`/`$session` structurals (§11.1). Unlike a plain [`CompiledView`] this
/// cannot be evaluated argument-free: its parameters and actor identity are
/// supplied per read by [`Engine::view_with`](crate::Engine::view_with). It is
/// addressed by its dotted surface path (`public.<name>`, `<role>.<name>`).
pub(crate) struct CompiledSurfaceView {
    pub(crate) address: String,
    pub(crate) expr: TypedExpr,
    pub(crate) params: Vec<CompiledParam>,
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

/// A compiled keyring declaration (§17.1): the ring name and its parsed policy.
/// The live version lifecycle is engine state built from this at load; the
/// declaration only carries the observable policy a provider must satisfy.
pub(crate) struct CompiledKeyring {
    pub(crate) name: String,
    pub(crate) policy: crate::keyring::KeyringPolicy,
}

/// The compiled artefacts the engine reuses across requests.
pub(crate) struct Compiled {
    pub(crate) collections: Vec<CompiledCollection>,
    /// Root-level computed values (§5.2) declared directly under `$model`, folded
    /// onto the package-root row at materialization so a view or projection reads
    /// them like any collection or stored value.
    pub(crate) root_computed: Vec<CompiledComputed>,
    pub(crate) mutations: Vec<CompiledMutation>,
    pub(crate) views: Vec<CompiledView>,
    /// Compiled `$public`/role surface `$view`s (§10.1), each carrying its
    /// `$params` and the `$actor`/`$session` structurals in scope so a
    /// param-aware or role read type-checks. Served by
    /// [`Engine::view_with`](crate::Engine::view_with), not folded into the root.
    pub(crate) surface_views: Vec<CompiledSurfaceView>,
    pub(crate) buckets: Vec<CompiledBucket>,
    /// Compiled source-backed / recurring bucket collections (§14.4–§14.6): each
    /// derives its interval rows from a `$source` view rather than stored state.
    pub(crate) source_buckets: Vec<crate::source_bucket::CompiledSourceBucket>,
    /// Compiled `$limits`/`$consumes` meter declarations (§15): pool sources,
    /// eligibility, order, and each spend collection's amount/time/metadata.
    pub(crate) meters: crate::meter::CompiledMeters,
    /// Declared keyrings (§17.1): the rings the engine bootstraps a live version
    /// lifecycle for and materializes a version view under.
    pub(crate) keyrings: Vec<CompiledKeyring>,
    /// The declaration-name path of the collection an authenticator selects as
    /// `$actor` (§11.3), so an authenticated admission re-materializes that row by
    /// key. `None` when no `$auth` declares a resolvable `$actor` collection.
    pub(crate) actor_collection: Option<Vec<String>>,
    /// The declaration-name path of the collection an authenticator selects as
    /// `$session` (§11.3), or `None` when no authenticator declares one.
    pub(crate) session_collection: Option<Vec<String>>,
}

impl Compiled {
    /// Compile a validated model against its source document.
    pub(crate) fn build(
        sources: &mut SourceMap,
        model: &Model,
        model_doc: &liasse_syntax::DocValue,
        precision: liasse_value::Precision,
    ) -> Result<Self, EngineError> {
        let schema = Schema::new(model);
        let root_ty = ExprType::Row(schema.root_row_type());
        let auth = AuthBindings::derive(schema, model_doc);
        let mut collections = compile_collections(sources, schema, &root_ty)?;
        // §4.4: apply the declared `timestamp_precision` to every stored `timestamp`
        // field type, so a seed or mutation decodes a bare wire count at the package
        // precision (the model keeps the default microsecond precision on field
        // types). Interval bounds and meter times then compare at the intended scale.
        for collection in &mut collections {
            apply_precision(collection, precision);
        }
        let root_computed = compile_root_computed(sources, schema, &root_ty)?;
        let mutations = compile_mutations(sources, schema, &root_ty, model_doc, &auth)?;
        let keyrings = compile_keyrings(schema, model_doc);
        let views = compile_views(sources, schema, &root_ty, &keyrings, model_doc)?;
        let surface_views = compile_surface_views(sources, schema, &root_ty, model_doc, &auth);
        let buckets = compile_buckets(sources, schema, &root_ty, model_doc)?;
        let source_buckets = crate::source_bucket::compile(sources, schema, &root_ty, model_doc)?;
        let meters = crate::meter::compile(sources, schema, &root_ty, model_doc)?;
        Ok(Self {
            collections,
            root_computed,
            mutations,
            views,
            surface_views,
            buckets,
            source_buckets,
            meters,
            keyrings,
            actor_collection: auth.actor.map(|(path, _)| path),
            session_collection: auth.session.map(|(path, _)| path),
        })
    }

    /// The compiled top-level collection named `name`, if any.
    pub(crate) fn collection(&self, name: &str) -> Option<&CompiledCollection> {
        self.collections.iter().find(|c| c.name == name)
    }

    /// The compiled collection at a declaration-name path (`["companies"]` or
    /// `["companies", "offices"]`), descending nested collections (§5.4).
    pub(crate) fn collection_at(&self, path: &[String]) -> Option<&CompiledCollection> {
        let (head, rest) = path.split_first()?;
        self.collection(head)?.at(rest)
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

    /// The compiled collection named `name` at any depth (§5.4): the top-level
    /// collection, else the first nested collection of that declaration name. Used
    /// by the temporal machinery to evaluate a bucketed pool row's interval bounds
    /// whether the bucket is top-level or a nested meter pool.
    pub(crate) fn find_collection(&self, name: &str) -> Option<&CompiledCollection> {
        if let Some(top) = self.collection(name) {
            return Some(top);
        }
        fn descend<'a>(collection: &'a CompiledCollection, name: &str) -> Option<&'a CompiledCollection> {
            for child in &collection.children {
                if child.name == name {
                    return Some(child);
                }
                if let Some(found) = descend(child, name) {
                    return Some(found);
                }
            }
            None
        }
        self.collections.iter().find_map(|c| descend(c, name))
    }

    /// The compiled surface `$view` at dotted `address` (`public.<name>`,
    /// `<role>.<name>`), if one is declared (§10.1).
    pub(crate) fn surface_view(&self, address: &str) -> Option<&CompiledSurfaceView> {
        self.surface_views.iter().find(|v| v.address == address)
    }
}

pub(crate) fn compile_expr(
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

/// Compile a reference's `$on_delete` policy (§21.1). A `= { … }` patch is typed
/// against the referencing row (`.`) with the deleted target bound as the
/// structural `$target`, so a patch may copy fields off the vanishing row.
fn compile_on_delete(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    row_ty: &ExprType,
    target: &str,
    reference: &liasse_model::Reference,
) -> Result<OnDelete, EngineError> {
    let Some(source) = &reference.on_delete else {
        return Ok(OnDelete::Undecided);
    };
    let text = source.text.trim();
    match text {
        "restrict" => Ok(OnDelete::Restrict),
        "cascade" => Ok(OnDelete::Cascade),
        "none" => Ok(OnDelete::Clear),
        _ if text.starts_with('=') => {
            let body = text[1..].trim();
            let target_ty = schema
                .receiver_row_type(std::slice::from_ref(&target.to_owned()))
                .unwrap_or_else(|| ExprType::Row(RowType::keyless(std::iter::empty())));
            let scope = RuntimeScope::new(row_ty.clone(), root_ty.clone()).with_structural("target", target_ty);
            let (patch, _) = compile_expr(sources, &scope, "on-delete", body)?;
            Ok(OnDelete::Patch(Box::new(patch)))
        }
        other => Err(EngineError::Internal(format!("unrecognized `$on_delete` policy `{other}`"))),
    }
}

/// Rewrite every stored `timestamp` field type of `collection` (and its structs
/// and nested collections) to the declared package precision (§4.4).
fn apply_precision(collection: &mut CompiledCollection, precision: liasse_value::Precision) {
    for field in &mut collection.fields {
        field.ty = retimestamp(&field.ty, precision);
    }
    for structure in &mut collection.structs {
        for field in &mut structure.fields {
            field.ty = retimestamp(&field.ty, precision);
        }
    }
    for child in &mut collection.children {
        apply_precision(child, precision);
    }
}

/// A copy of `ty` with every `timestamp` (bare, optional, or set element) carrying
/// `precision` (§4.4). Non-timestamp types are unchanged.
fn retimestamp(ty: &Type, precision: liasse_value::Precision) -> Type {
    match ty {
        Type::Timestamp(_) => Type::Timestamp(precision),
        Type::Optional(inner) => Type::Optional(Box::new(retimestamp(inner, precision))),
        Type::Set(inner) => Type::Set(Box::new(retimestamp(inner, precision))),
        other => other.clone(),
    }
}

fn compile_collections(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
) -> Result<Vec<CompiledCollection>, EngineError> {
    let mut out = Vec::new();
    for member in &schema.model().root().members {
        if let Node::Collection(collection) = &member.node {
            let path = vec![member.name.as_str().to_owned()];
            out.push(compile_collection(sources, schema, root_ty, &path, collection)?);
        }
    }
    Ok(out)
}

fn compile_collection(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    path: &[String],
    collection: &Collection,
) -> Result<CompiledCollection, EngineError> {
    let name = path.last().map_or("", String::as_str);
    let row_ty = schema
        .receiver_row_type(path)
        .unwrap_or_else(|| ExprType::Row(RowType::keyless(std::iter::empty())));
    let row_scope = RuntimeScope::new(row_ty.clone(), root_ty.clone());

    let mut fields = Vec::new();
    let mut computed = Vec::new();
    let mut structs = Vec::new();
    let mut children = Vec::new();
    let mut unique: Vec<Vec<String>> = collection
        .unique
        .iter()
        .map(|group| group.iter().map(|f| f.as_str().to_owned()).collect())
        .collect();

    for member in &collection.shape.members {
        let name = member.name.as_str();
        match &member.node {
            // A static struct member (§5.3): compiled so its own field defaults,
            // normalizers, and checks run during the containing insertion (§5.1).
            Node::Struct(shape) => {
                let mut child_path = path.to_vec();
                child_path.push(name.to_owned());
                structs.push(compile_struct(sources, schema, root_ty, &child_path, name, shape)?);
            }
            // A nested keyed collection (§5.4): compiled recursively into a child.
            Node::Collection(nested) => {
                let mut child_path = path.to_vec();
                child_path.push(name.to_owned());
                children.push(compile_collection(sources, schema, root_ty, &child_path, nested)?);
            }
            _ => {
                if let Some(field) =
                    compile_field(sources, schema, root_ty, &row_ty, &row_scope, member, &mut unique, &mut computed)?
                {
                    fields.push(field);
                }
            }
        }
    }

    let row_checks = compile_checks(sources, &row_scope, "row-check", &collection.shape.checks)?;

    Ok(CompiledCollection {
        name: name.to_owned(),
        key: collection.key.iter().map(|f| f.as_str().to_owned()).collect(),
        unique,
        fields,
        computed,
        row_checks,
        structs,
        children,
    })
}

/// Compile a static struct member (§5.3): its writable fields (with defaults,
/// normalizers, and checks) and its struct-level `$check`s, so a supplied struct
/// initializer resolves omitted defaults and is validated with the row (§5.1,
/// §5.10). Nested collections inside a struct remain a documented seam.
fn compile_struct(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    path: &[String],
    name: &str,
    shape: &Shape,
) -> Result<CompiledStruct, EngineError> {
    let row_ty = schema
        .receiver_row_type(path)
        .unwrap_or_else(|| ExprType::Row(RowType::keyless(std::iter::empty())));
    let row_scope = RuntimeScope::new(row_ty.clone(), root_ty.clone());
    let mut fields = Vec::new();
    let mut unique = Vec::new();
    let mut computed = Vec::new();
    for member in &shape.members {
        if let Node::Scalar(_) | Node::Reference(_) | Node::Set(_) = &member.node
            && let Some(field) =
                compile_field(sources, schema, root_ty, &row_ty, &row_scope, member, &mut unique, &mut computed)?
        {
            fields.push(field);
        }
    }
    let row_checks = compile_checks(sources, &row_scope, "struct-check", &shape.checks)?;
    Ok(CompiledStruct { name: name.to_owned(), fields, row_checks })
}

/// Compile one writable field (scalar, reference, or set) of a row or struct
/// shape, or `None` for a read-only computed value (which is accumulated into
/// `computed` instead). A `$unique: true` scalar appends a single-field
/// candidate key to `unique`.
#[allow(clippy::too_many_arguments)]
fn compile_field(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    row_ty: &ExprType,
    row_scope: &RuntimeScope,
    member: &liasse_model::Member,
    unique: &mut Vec<Vec<String>>,
    computed: &mut Vec<CompiledComputed>,
) -> Result<Option<CompiledField>, EngineError> {
    let name = member.name.as_str().to_owned();
    let field = match &member.node {
        // A read-only computed value is not an insertable field (§5.2).
        Node::Scalar(scalar) if !scalar.is_writable() => {
            if let Some(source) = &scalar.computed {
                let (expr, _src) = compile_expr(sources, row_scope, "computed", &source.text)?;
                computed.push(CompiledComputed { name, expr });
            }
            return Ok(None);
        }
        Node::Scalar(scalar) => {
            if scalar.unique {
                unique.push(vec![name.clone()]);
            }
            let default = match &scalar.default {
                Some(source) => Some(compile_expr(sources, row_scope, "default", &source.text)?),
                None => None,
            };
            let field_scope = RuntimeScope::new(ExprType::scalar(scalar.ty.clone()), root_ty.clone());
            let normalize = match &scalar.normalize {
                Some(source) => Some(compile_expr(sources, &field_scope, "normalize", &source.text)?),
                None => None,
            };
            let checks = compile_checks(sources, &field_scope, "check", &scalar.checks)?;
            CompiledField { name, ty: scalar.ty.clone(), reference: None, default, normalize, checks }
        }
        Node::Reference(reference) => {
            let target = reference.target.trim_start_matches('/').to_owned();
            let on_delete = compile_on_delete(sources, schema, root_ty, row_ty, &target, reference)?;
            CompiledField {
                name,
                ty: Type::Ref(liasse_value::RefTarget::Scalar(Box::new(reference.key_type.clone()))),
                reference: Some(RefInfo { target, optional: reference.optional, on_delete }),
                default: None,
                normalize: None,
                checks: Vec::new(),
            }
        }
        Node::Set(set) => CompiledField {
            name,
            ty: Type::Set(Box::new(set.element.clone())),
            reference: None,
            default: None,
            normalize: None,
            checks: Vec::new(),
        },
        _ => return Ok(None),
    };
    Ok(Some(field))
}

/// The `$actor`/`$session` bindings an authenticated admission introduces
/// (§11.1), each the collection declaration path plus the row type a program
/// reading `$actor`/`$session` type-checks against. Derived from the package's
/// `$auth` declarations; the model validates those declarations but leaves the
/// target-collection resolution a documented seam (`liasse-model/src/auth.rs`),
/// so the runtime re-derives it from the definition document it already holds.
struct AuthBindings {
    actor: Option<(Vec<String>, ExprType)>,
    session: Option<(Vec<String>, ExprType)>,
}

impl AuthBindings {
    /// Resolve the actor/session collections from the first authenticator that
    /// names each. Every authenticator a role accepts must resolve a compatible
    /// `$actor` (§10.3), so one representative type is enough to type-check a
    /// program; the actual row is re-materialized per request from the key the
    /// request carries.
    fn derive(schema: Schema<'_>, model_doc: &liasse_syntax::DocValue) -> Self {
        let mut bindings = Self { actor: None, session: None };
        let Some(auth) = doc::member(model_doc, "$auth").and_then(doc::object) else {
            return bindings;
        };
        for authenticator in auth {
            let def = &authenticator.value;
            if bindings.actor.is_none() {
                bindings.actor = resolve_selection(schema, doc::member(def, "$actor"));
            }
            if bindings.session.is_none() {
                bindings.session = resolve_selection(schema, doc::member(def, "$session"));
            }
        }
        bindings
    }

    /// The `(name, row type)` structurals a mutation program admitted through this
    /// package's authenticators has in scope (§6.2).
    fn structurals(&self) -> Vec<(String, ExprType)> {
        let mut out = Vec::new();
        if let Some((_, ty)) = &self.actor {
            out.push(("actor".to_owned(), ty.clone()));
        }
        if let Some((_, ty)) = &self.session {
            out.push(("session".to_owned(), ty.clone()));
        }
        out
    }
}

/// The `(collection path, row type)` a `/collection[selector]` authenticator
/// selection addresses, if it names a declared top-level collection. A selection
/// the runtime cannot resolve to a collection (an absent member, a non-string, a
/// nested or computed target) leaves that binding unavailable, so a program
/// reading it fails to type-check — the same closed-world refusal as an unknown
/// structural.
fn resolve_selection(schema: Schema<'_>, selection: Option<&liasse_syntax::DocValue>) -> Option<(Vec<String>, ExprType)> {
    let text = doc::string(selection?)?;
    let rest = text.trim().strip_prefix('/')?;
    let end = rest.find('[').unwrap_or(rest.len());
    let name = rest.get(..end)?.trim();
    if name.is_empty() {
        return None;
    }
    let path = vec![name.to_owned()];
    let ty = schema.receiver_row_type(&path)?;
    matches!(ty, ExprType::Row(_)).then_some((path, ty))
}

fn compile_mutations(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
    auth: &AuthBindings,
) -> Result<Vec<CompiledMutation>, EngineError> {
    let context_structurals = auth.structurals();
    let mut out = Vec::new();
    for mutation in schema.model().mutations() {
        let receiver_ty = schema
            .receiver_row_type(&mutation.path)
            .ok_or_else(|| EngineError::Internal(format!("mutation `{}` has no receiver", mutation.name.as_str())))?;
        let mut scope = RuntimeScope::new(receiver_ty, root_ty.clone());
        for (name, ty) in &mutation.params {
            scope = scope.with_param(name.clone(), ty.clone());
        }
        // §11.1/§6.2: `$actor` (and `$session`, when declared) are in scope for a
        // mutation program, resolved per request from the admitting authenticator.
        for (name, ty) in &context_structurals {
            scope = scope.with_structural(name.clone(), ty.clone());
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
            context_structurals: context_structurals.clone(),
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

/// Compile each root-level computed value (§5.2): a non-writable scalar member
/// declared directly under `$model`, typed with the package root as its receiver
/// (`.` = root), so `n: "= count(.items)"` reads sibling collections and other
/// root computed values.
fn compile_root_computed(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
) -> Result<Vec<CompiledComputed>, EngineError> {
    let scope = RuntimeScope::new(root_ty.clone(), root_ty.clone());
    let mut out = Vec::new();
    for member in &schema.model().root().members {
        if let Node::Scalar(scalar) = &member.node
            && let Some(source) = &scalar.computed
        {
            let (expr, _src) = compile_expr(sources, &scope, "computed", &source.text)?;
            out.push(CompiledComputed { name: member.name.as_str().to_owned(), expr });
        }
    }
    Ok(out)
}

fn compile_views(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    keyrings: &[CompiledKeyring],
    model_doc: &liasse_syntax::DocValue,
) -> Result<Vec<CompiledView>, EngineError> {
    let scope = RuntimeScope::new(root_ty.clone(), root_ty.clone());
    let mut out = Vec::new();
    for member in &schema.model().root().members {
        if let Node::View(view) = &member.node {
            let name = member.name.as_str();
            // §17.2: a keyring is projected as a `Node::View` for typing, but its
            // rows are the version metadata the engine materializes directly, not
            // its stand-in `.` expression. Skip it so the keyring materializer owns
            // the ring member rather than an evaluated whole-root clone.
            if keyrings.iter().any(|k| k.name == name) {
                continue;
            }
            // §14.4: a source-backed bucket is also projected as a `Node::View`, but
            // its rows are materialized from its `$source` view (not its placeholder
            // `.` expression); the source-bucket materializer owns the member.
            if crate::source_bucket::is_source_bucket(model_doc, name) {
                continue;
            }
            let (expr, _source) = compile_expr(sources, &scope, "view", &view.expr.text)?;
            out.push(CompiledView { name: name.to_owned(), expr });
        }
    }
    Ok(out)
}

/// Parse each `$keyring` declaration (§17.1) into its policy. The model projects
/// a keyring as a `Node::View`, so the declaration is read from the source
/// document (the model retains no policy); a declaration the parser cannot read
/// is dropped, leaving the ring without a live lifecycle rather than failing the
/// load.
fn compile_keyrings(schema: Schema<'_>, model_doc: &liasse_syntax::DocValue) -> Vec<CompiledKeyring> {
    let mut out = Vec::new();
    for member in &schema.model().root().members {
        if !matches!(&member.node, Node::View(_)) {
            continue;
        }
        let name = member.name.as_str().to_owned();
        let Some(shape) = doc::shape_at(model_doc, std::slice::from_ref(&name)) else { continue };
        let Some(keyring) = doc::member(shape, "$keyring") else { continue };
        if let Some(policy) = crate::keyring_view::policy_from_doc(keyring) {
            out.push(CompiledKeyring { name, policy });
        }
    }
    out
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
    // Lifecycle buckets at every level (§14.1–§14.2), top-level and nested. A nested
    // bucketed collection is a §15 meter pool (bucketed `topups`): registering it is
    // what lets a meter source read only the pool rows active at the spend instant
    // (§15.1), so each bucketed pool row carries its `$from`/`$until` interval cells.
    // Source-backed and recurring buckets (`$source`/`$repeat`) are compiled
    // separately ([`crate::source_bucket`]).
    for member in &schema.model().root().members {
        if let Node::Collection(collection) = &member.node {
            let path = vec![member.name.as_str().to_owned()];
            compile_buckets_at(sources, schema, root_ty, model_doc, &path, collection, &mut out)?;
        }
    }
    Ok(out)
}

/// Compile the `$bucket` of the collection at declaration `path`, then recurse
/// into its nested collections (§5.4), so a bucketed pool at any depth registers.
fn compile_buckets_at(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
    path: &[String],
    collection: &Collection,
    out: &mut Vec<CompiledBucket>,
) -> Result<(), EngineError> {
    let name = path.last().map_or("", String::as_str);
    if let Some(shape) = doc::shape_at(model_doc, path)
        && let Some(bucket_doc) = doc::member(shape, "$bucket")
    {
        let row_ty = schema
            .receiver_row_type(path)
            .unwrap_or_else(|| ExprType::Row(RowType::keyless(std::iter::empty())));
        let scope = RuntimeScope::new(row_ty, root_ty.clone());
        if let Some(bucket) = compile_bucket(sources, &scope, name, bucket_doc)? {
            out.push(bucket);
        }
    }
    for member in &collection.shape.members {
        if let Node::Collection(nested) = &member.node {
            let mut child = path.to_vec();
            child.push(member.name.as_str().to_owned());
            compile_buckets_at(sources, schema, root_ty, model_doc, &child, nested, out)?;
        }
    }
    Ok(())
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

/// Compile every `$public` and role surface `$view` (§10.1) into a
/// [`CompiledSurfaceView`] the param- and actor-aware read
/// [`Engine::view_with`](crate::Engine::view_with) serves. The model validates
/// these surfaces but retains only their call contracts (`liasse-model/src/surface.rs`),
/// so — like buckets and meters — the declarations are read from the document.
///
/// The scope carries the surface `$params` (read as `@name`) and the package's
/// `$actor`/`$session` structurals (§11.1), so a param-aware or role `$view`
/// type-checks. A surface whose `$view` does not compile (an unrepresentable
/// param type, or an expression the runtime cannot yet type) is dropped rather
/// than failing the whole load, leaving that surface unserved.
fn compile_surface_views(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
    auth: &AuthBindings,
) -> Vec<CompiledSurfaceView> {
    let mut out = Vec::new();
    let structurals = auth.structurals();
    if let Some(public) = doc::member(model_doc, "$public").and_then(doc::object) {
        for surface in public {
            compile_one_surface_view(
                sources,
                root_ty,
                &format!("public.{}", surface.name.text),
                &surface.value,
                &structurals,
                &mut out,
            );
        }
    }
    if let Some(roles) = doc::member(model_doc, "$roles").and_then(doc::object) {
        for role in roles {
            let Some(members) = doc::object(&role.value) else { continue };
            for member in members {
                // A role's `$`-members (`$members`/`$auth`/`$recursive`) are not
                // granted surfaces; its plain members are (§10.4).
                if member.name.text.starts_with('$') {
                    continue;
                }
                compile_one_surface_view(
                    sources,
                    root_ty,
                    &format!("{}.{}", role.name.text, member.name.text),
                    &member.value,
                    &structurals,
                    &mut out,
                );
            }
        }
    }
    let _ = schema;
    out
}

/// Compile one surface declaration's `$view` (§10.1) at dotted `address`, adding
/// it to `out` when it carries a compilable `$view`. A surface with only `$mut`
/// calls contributes nothing here.
fn compile_one_surface_view(
    sources: &mut SourceMap,
    root_ty: &ExprType,
    address: &str,
    surface: &liasse_syntax::DocValue,
    structurals: &[(String, ExprType)],
    out: &mut Vec<CompiledSurfaceView>,
) {
    let Some(members) = doc::object(surface) else { return };
    let Some(view_text) = members.iter().find(|m| m.name.text == "$view").and_then(|m| doc::string(&m.value))
    else {
        return;
    };
    let params = match compile_surface_params(sources, root_ty, surface) {
        Some(params) => params,
        None => return,
    };
    let mut scope = RuntimeScope::new(root_ty.clone(), root_ty.clone());
    for param in &params {
        scope = scope.with_param(param.name.clone(), param.ty.clone());
    }
    for (name, ty) in structurals {
        scope = scope.with_structural(name.clone(), ty.clone());
    }
    if let Ok((expr, _)) = compile_expr(sources, &scope, "surface-view", view_text) {
        out.push(CompiledSurfaceView { address: address.to_owned(), expr, params });
    }
}

/// Compile a surface's `$params` declarations (§10.1) into typed
/// [`CompiledParam`]s, or `None` when a declared parameter type is
/// unrepresentable (so the whole surface view is dropped rather than mis-typed).
fn compile_surface_params(
    sources: &mut SourceMap,
    root_ty: &ExprType,
    surface: &liasse_syntax::DocValue,
) -> Option<Vec<CompiledParam>> {
    let Some(params_member) = doc::member(surface, "$params") else {
        return Some(Vec::new());
    };
    let members = doc::object(params_member)?;
    let mut out = Vec::new();
    for member in members {
        let (ty, default_text) = param_type_and_default(&member.value)?;
        let scope = RuntimeScope::new(root_ty.clone(), root_ty.clone());
        let default = match default_text {
            Some(text) => Some(compile_expr(sources, &scope, "param-default", &text).ok()?.0),
            None => None,
        };
        out.push(CompiledParam { name: member.name.text.clone(), ty, default });
    }
    Some(out)
}

/// The declared type and optional default text of one `$params` entry (§10.1):
/// either a bare type string (`"bool = false"`, `"text?"`) or the expanded
/// object form carrying `$type`/`$default`/`$optional` (A.3). `None` when the
/// type is not representable as a scalar/optional/set contract.
fn param_type_and_default(value: &liasse_syntax::DocValue) -> Option<(ExprType, Option<String>)> {
    if let Some(text) = doc::string(value) {
        let (type_str, default) = match text.split_once('=') {
            Some((lhs, rhs)) => (lhs.trim(), Some(rhs.trim().to_owned())),
            None => (text.trim(), None),
        };
        return Some((ExprType::scalar(lower_scalar_type(type_str)?), default));
    }
    let type_text = doc::string(doc::member(value, "$type")?)?;
    let optional = doc::member(value, "$optional")
        .and_then(doc::bool_value)
        .unwrap_or(false);
    let mut ty = lower_scalar_type(type_text.trim())?;
    if optional {
        ty = Type::Optional(Box::new(ty));
    }
    let default = doc::member(value, "$default").and_then(doc::string).map(str::to_owned);
    Some((ExprType::scalar(ty), default))
}

/// Lower an A.2 scalar/optional/set type spelling to a [`Type`] for a surface
/// parameter's contract. A named (`$types`) or `ref` parameter type is not
/// resolved here (it is uncommon in `$params`), so it yields `None` and drops the
/// surface view as a documented seam rather than mis-typing it.
fn lower_scalar_type(text: &str) -> Option<Type> {
    use liasse_syntax::{parse_type_expression, SpannedType, TypeExprKind};
    fn lower(node: &SpannedType) -> Option<Type> {
        match &node.kind {
            TypeExprKind::Name(word) => match word.as_str() {
                "text" => Some(Type::Text),
                "bool" => Some(Type::Bool),
                "int" => Some(Type::Int),
                "decimal" => Some(Type::Decimal),
                "bytes" => Some(Type::Bytes),
                "uuid" => Some(Type::Uuid),
                "date" => Some(Type::Date),
                "timestamp" => Some(Type::timestamp()),
                "duration" => Some(Type::Duration),
                "period" => Some(Type::Period),
                "json" => Some(Type::Json),
                "blob" => Some(Type::Blob),
                _ => None,
            },
            TypeExprKind::OptionalSuffix(inner) | TypeExprKind::Optional(inner) => {
                let inner = lower(inner)?;
                (!matches!(inner, Type::Optional(_))).then(|| Type::Optional(Box::new(inner)))
            }
            TypeExprKind::Set(inner) => Some(Type::Set(Box::new(lower(inner)?))),
            _ => None,
        }
    }
    let mut sources = SourceMap::new();
    let id = sources.add_label("param-type", text.to_owned());
    let spanned = parse_type_expression(id, text).ok()?;
    lower(&spanned)
}
