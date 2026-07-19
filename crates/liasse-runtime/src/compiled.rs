//! Load-time compilation: the once-per-definition work the admission hot path
//! reuses.
//!
//! The model proves the definition statically valid but keeps neither the typed
//! programs nor the seed. Rather than re-parse and re-type-check on every
//! request, the engine compiles — once, at load — each collection's defaults,
//! normalizers, and checks into typed expressions, each mutation's statement
//! program with its parameter scope, and each view into a typed expression. The
//! result is an owned [`Compiled`] the engine holds beside the model and store.

use std::collections::BTreeSet;

use liasse_diag::{Diagnostic, Diagnostics, SourceId, SourceMap, Span};
use liasse_expr::{check_statement, ExprType, HostPosition, RowType, Scope, SortOrder, TypedExpr, ViewOrders};
use liasse_host::KeyOperation;
use liasse_model::{Collection, Model, Node, Shape};
use liasse_syntax::{
    parse_expression, Arg, BlockMember, BlockMemberKind, Expr, ExprKind, Selector, Stmt, StmtKind,
};
use liasse_value::{StructType, Type};

use crate::doc;
use crate::error::EngineError;
use crate::host::HostSignatures;
use crate::recursion::{CompiledRecursive, CompiledScope};
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
    /// The reference target when this field is a scalar `$ref` (§5.6): its value
    /// is one target key that must resolve.
    pub(crate) reference: Option<RefInfo>,
    /// The reference target when this field is a `$set` of `$ref` (§5.5): each
    /// set member is a reference that must resolve (§5.6). The model flattens a
    /// set element to a bare `ref<T>` type and drops the target relation, so the
    /// target is recovered from the definition document at compile time and kept
    /// here — the set analogue of [`Self::reference`] for member-level integrity
    /// and atomic-rekey rewrite (§5.4).
    pub(crate) element_reference: Option<RefInfo>,
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

/// A writable singleton root field's insertion default (§8.2): its name and the
/// typed default expression evaluated over the package root (`.` = the root row).
/// Applied once at genesis so a root field declared `= …` takes its default value
/// when `$data` supplies none — the singleton analogue of a collection field's
/// default.
pub(crate) struct CompiledSingletonDefault {
    pub(crate) name: String,
    pub(crate) default: TypedExpr,
}

/// A writable singleton root field's normalizer (§8.2/§8.8): its name and the
/// typed `$normalize` expression, typed with the member's own value as `.` over
/// the package root. Applied every time the member is written — at a seed, at a
/// resolved default, and at a `.field = …` mutation — the singleton analogue of a
/// collection field's [`CompiledField::normalize`].
pub(crate) struct CompiledSingletonNormalize {
    pub(crate) name: String,
    pub(crate) normalize: TypedExpr,
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
    /// Nested `$view` members (§7.1) declared under this collection's rows, each
    /// typed with the row as `.` — the row-scoped analogue of a root view. Folded
    /// onto each row at materialization so a `/coll[k].view` read resolves; the
    /// canonical case is a `catalog: ".modules::iface { … }"` §13.9 aggregation.
    pub(crate) views: Vec<CompiledView>,
}

/// A compiled static struct member (§5.3): a plain nested object sharing the
/// containing row's identity and lifecycle. Its fields carry their own defaults
/// and normalizers, resolved during the containing insertion (§5.1).
pub(crate) struct CompiledStruct {
    pub(crate) name: String,
    pub(crate) fields: Vec<CompiledField>,
    pub(crate) row_checks: Vec<CompiledCheck>,
}

impl CompiledStruct {
    /// The raw `Type::Struct` this static struct member declares (§5.3), over its
    /// compiled fields in field-name text order — the same field-name-ordered
    /// struct `Type` the portable state codec and Annex E identity comparison build
    /// on. Raw here means undecorated: the optional-wrapping the artifact decoder
    /// applies (`singleton::optional_decode_struct`) is layered on top by the caller.
    pub(crate) fn ty(&self) -> Type {
        Type::Struct(StructType::new(self.fields.iter().map(|field| (field.name.clone(), field.ty.clone()))))
    }
}

impl CompiledCollection {
    /// The field descriptor named `name`, if declared.
    pub(crate) fn field(&self, name: &str) -> Option<&CompiledField> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// The `Type::Struct` a static struct member named `name` declares (§5.3),
    /// reconstructed from its compiled fields in field-name text order — the same
    /// field-name-ordered struct `Type` the model's key builder produces for a
    /// struct-typed `$key` (A.8, `Shape::key_struct_type`). A struct member
    /// compiles into [`Self::structs`], not [`Self::fields`], so [`Self::field`]
    /// never resolves a struct-typed `$key` component; this recovers its real type
    /// for the Annex E exposed-row-identity comparison (A.9/E.5). `None` when no
    /// struct member of that name is declared.
    pub(crate) fn struct_type(&self, name: &str) -> Option<Type> {
        Some(self.structs.iter().find(|s| s.name == name)?.ty())
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

/// Resolves a referenced view's order against the compiled registry (§7.1/§7.4)
/// for [`Compiled::view_order_of`]. `fuel` bounds the reference chain: each hop
/// into a referenced view spends one unit, so a chain longer than the view count —
/// necessarily a cycle — runs out and resolves to occurrence identity (`None`)
/// rather than recursing forever. No interior mutability: each deeper resolution
/// carries its own decremented `fuel`.
struct ViewOrderResolver<'c> {
    compiled: &'c Compiled,
    fuel: usize,
}

impl ViewOrders for ViewOrderResolver<'_> {
    fn view_order(&self, name: &str) -> Option<SortOrder> {
        let view = self.compiled.view(name)?;
        let fuel = self.fuel.checked_sub(1)?;
        Some(view.expr.result_order(&ViewOrderResolver { compiled: self.compiled, fuel }))
    }
}

/// A compiled exposed module interface `$view` (§13.8): the interface handle name
/// a parent or peer addresses (`::templates`) and the typed projection over the
/// module's own root. Only the fields this projection selects cross the boundary —
/// a private field it omits is unreachable through the interface (§13.8 isolation),
/// so an interface-addressed read evaluates this expression rather than any private
/// child view or path.
pub(crate) struct CompiledExposed {
    pub(crate) interface: String,
    pub(crate) expr: TypedExpr,
}

/// A declared `$modules` space (§13.2): the declaration-name path of the space
/// node (`["companies", "modules"]`) and the interface contracts it declares. The
/// path tells the root engine which rows to fold installed instances into (§13.9);
/// the contracts are the boundary a child's `$expose` must satisfy at install
/// (§13.8).
pub(crate) struct CompiledModuleSpace {
    /// The declaration-name path of the `$modules` node from `$model`.
    pub(crate) path: Vec<String>,
    /// The interface contracts the space declares (`$interfaces`), each the boundary
    /// a child exposing that interface must satisfy structurally (§13.8).
    pub(crate) interfaces: Vec<CompiledInterfaceContract>,
}

/// One `$interfaces` boundary contract of a module space (§13.8): the interface
/// name and the `$view` fields it requires an exposing child to project. A child
/// whose exposed `$view` omits a required field, or projects one the contract does
/// not declare, does not structurally satisfy the interface.
pub(crate) struct CompiledInterfaceContract {
    /// The interface name a parent addresses (`::templates`).
    pub(crate) name: String,
    /// The `(field, type)` pairs the interface `$view` declares (§13.8). Empty when
    /// the interface declares no `$view` (a mutation-only interface).
    pub(crate) view_fields: Vec<(String, Type)>,
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
    /// The scope binding of a scoped-role surface view (§10.3/§10.5): the covered
    /// row `.` resolves to the collection row keyed by the request scope, and — when
    /// declared — the `$recursive` coverage that nests the same projection through a
    /// descendant relation (§10.5). `None` for a `$public`/package-level view, whose
    /// `.` is the package root.
    pub(crate) scope: Option<crate::recursion::CompiledScope>,
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
    /// The root singleton row (§8.2) compiled as a pseudo-collection over the
    /// root's writable members, so the admission pipeline validates it exactly
    /// like a keyed row — field/row checks, reference integrity (scalar and
    /// set-of-ref members), and uniqueness all hold on the singleton row (§22.1).
    /// Resolved by [`Self::collection_at`] under the reserved `$root` path for the
    /// final rule pass and the atomic-rekey inbound-ref rewrite (§5.4).
    pub(crate) root_singleton: CompiledCollection,
    /// Root-level computed values (§5.2) declared directly under `$model`, folded
    /// onto the package-root row at materialization so a view or projection reads
    /// them like any collection or stored value.
    pub(crate) root_computed: Vec<CompiledComputed>,
    /// Normalizers for writable singleton root fields (§8.2/§8.8), applied every
    /// time the member is written (seed, default, mutation) — the singleton
    /// analogue of a collection field's normalizer.
    pub(crate) root_singleton_normalizes: Vec<CompiledSingletonNormalize>,
    /// Insertion defaults for writable singleton root fields (§8.2), applied at
    /// genesis when `$data` supplies no value.
    pub(crate) root_singleton_defaults: Vec<CompiledSingletonDefault>,
    pub(crate) mutations: Vec<CompiledMutation>,
    pub(crate) views: Vec<CompiledView>,
    /// Compiled `$expose` interface `$view`s (§13.8), each the boundary projection
    /// a parent or peer reads through the interface handle. Evaluated by
    /// [`Engine::interface_read`](crate::Engine::interface_read) against a child
    /// instance; only the projected fields cross the boundary.
    pub(crate) exposed_views: Vec<CompiledExposed>,
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
    /// The declared `$modules` spaces (§13.2), each with its declaration path and
    /// interface contracts, so the root engine can fold installed instances into a
    /// `.modules::iface` read (§13.9) and check `$expose` satisfaction at install
    /// (§13.8). Empty when the package declares no module space.
    pub(crate) module_spaces: Vec<CompiledModuleSpace>,
}

impl Compiled {
    /// Compile a validated model against its source document.
    pub(crate) fn build(
        sources: &mut SourceMap,
        model: &Model,
        model_doc: &liasse_syntax::DocValue,
        precision: liasse_value::Precision,
        hosts: &HostSignatures,
    ) -> Result<Self, EngineError> {
        let schema = Schema::new(model);
        // §13.1: a module package binds `$config` in every authored expression. The
        // model already type-checked them against the declared struct; the runtime
        // re-checks over its own scopes, so carry the `$config` struct as a
        // structural binding on the root row — every scope built over this root
        // (the [`RuntimeScope::structural`] fallback) then resolves `$config`.
        let mut root = schema.root_row_type();
        if let Some(config) = model.config_schema() {
            root = root.with_structural([("config".to_owned(), ExprType::Row(config.row_type().clone()))]);
        }
        let root_ty = ExprType::Row(root);
        let auth = AuthBindings::derive(schema, model_doc);
        let mut collections = compile_collections(sources, schema, &root_ty, model_doc, hosts)?;
        // §4.4: apply the declared `timestamp_precision` to every stored `timestamp`
        // field type, so a seed or mutation decodes a bare wire count at the package
        // precision (the model keeps the default microsecond precision on field
        // types). Interval bounds and meter times then compare at the intended scale.
        for collection in &mut collections {
            apply_precision(collection, precision);
        }
        let root_computed = compile_root_computed(sources, schema, &root_ty, hosts)?;
        let root_singleton = compile_root_singleton(sources, schema, &root_ty, model_doc, hosts)?;
        let root_singleton_defaults = compile_root_singleton_defaults(sources, schema, &root_ty, hosts)?;
        let root_singleton_normalizes = compile_root_singleton_normalizes(sources, schema, &root_ty, hosts)?;
        let mutations = compile_mutations(sources, schema, &root_ty, model_doc, &auth, hosts)?;
        let keyrings = compile_keyrings(schema, model_doc);
        let views = compile_views(sources, schema, &root_ty, &keyrings, model_doc, hosts)?;
        let exposed_views = compile_exposed_views(sources, &root_ty, model, hosts)?;
        let surface_views = compile_surface_views(sources, schema, &root_ty, model_doc, &auth, hosts);
        let buckets = compile_buckets(sources, schema, &root_ty, model_doc)?;
        let source_buckets = crate::source_bucket::compile(sources, schema, &root_ty, model_doc)?;
        let meters = crate::meter::compile(sources, schema, &root_ty, model_doc)?;
        let module_spaces = compile_module_spaces(model_doc, &[]);
        Ok(Self {
            collections,
            root_singleton,
            root_computed,
            root_singleton_defaults,
            root_singleton_normalizes,
            mutations,
            views,
            exposed_views,
            surface_views,
            buckets,
            source_buckets,
            meters,
            keyrings,
            actor_collection: auth.actor.map(|(path, _)| path),
            session_collection: auth.session.map(|(path, _)| path),
            module_spaces,
        })
    }

    /// The compiled top-level collection named `name`, if any.
    pub(crate) fn collection(&self, name: &str) -> Option<&CompiledCollection> {
        self.collections.iter().find(|c| c.name == name)
    }

    /// The compiled collection at a declaration-name path (`["companies"]` or
    /// `["companies", "offices"]`), descending nested collections (§5.4).
    pub(crate) fn collection_at(&self, path: &[String]) -> Option<&CompiledCollection> {
        // §8.2: the reserved `$root` row is the singleton pseudo-collection, so the
        // final rule pass and the inbound-ref rewrite resolve it here rather than
        // among the application collections (it never carries an application name).
        if let [only] = path
            && only == crate::singleton::ROOT_NAME
        {
            return Some(&self.root_singleton);
        }
        let (head, rest) = path.split_first()?;
        self.collection(head)?.at(rest)
    }

    /// The compiled mutation named `name`, if any.
    pub(crate) fn mutation(&self, name: &str) -> Option<&CompiledMutation> {
        self.mutations.iter().find(|m| m.name == name)
    }

    /// The compiled `$normalize` of the writable singleton root field `name`, if
    /// it declares one (§8.2/§8.8).
    pub(crate) fn singleton_normalize(&self, name: &str) -> Option<&TypedExpr> {
        self.root_singleton_normalizes.iter().find(|n| n.name == name).map(|n| &n.normalize)
    }

    /// The compiled view named `name`, if any.
    pub(crate) fn view(&self, name: &str) -> Option<&CompiledView> {
        self.views.iter().find(|v| v.name == name)
    }

    /// The total order `expr` delivers its rows in (§7.3/§7.4), resolving every
    /// reference to a top-level named view through this registry — a `$view` is
    /// folded onto the root row as a same-named cell (§7.1), so a bare reference's
    /// typed node carries no `$sort` of its own and its order lives in the
    /// *referenced* view's definition. A bounded window partitions its frozen gap
    /// coordinate through exactly this order (§12.2), so a §7.4 combinator over a
    /// sorted left view must report that left order here for the partition to stay
    /// monotone. The reference chain is bounded by the view count: a chain longer
    /// than that repeats a view, which is a cycle that never materializes (§7.1
    /// fixed point), so it is cut back to occurrence identity.
    pub(crate) fn view_order_of(&self, expr: &TypedExpr) -> SortOrder {
        expr.result_order(&ViewOrderResolver { compiled: self, fuel: self.views.len() })
    }

    /// The compiled `$expose` interface `$view` for interface `name`, if one is
    /// declared with a readable projection (§13.8).
    pub(crate) fn exposed_view(&self, name: &str) -> Option<&TypedExpr> {
        self.exposed_views.iter().find(|e| e.interface == name).map(|e| &e.expr)
    }

    /// The `$interfaces` boundary contracts of the `$modules` space at declaration
    /// path `path` (§13.8), if the package declares one there. Used at install to
    /// check a child's `$expose` structurally satisfies the space's contract.
    pub(crate) fn module_space_interfaces(&self, path: &[String]) -> Option<&[CompiledInterfaceContract]> {
        self.module_spaces.iter().find(|space| space.path == path).map(|space| space.interfaces.as_slice())
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

/// The bound on how deep a self-referential `$types`/`$like` shape (§5.8) is
/// eagerly expanded into the compiled collection tree, matching the resolver's
/// recursion cap ([`crate::schema`]). The compiled tree is a finite structure, so
/// a type that refers to itself (`subcompanies: "company"`) cannot be expanded to
/// its infinite depth; it is compiled to this depth and cut, a documented CORE
/// bound identical to the one the model resolver applies when typing the same
/// shape. Reads (materialization) are bounded by the actual data depth instead, so
/// a tree deeper than any single mutation/seed still round-trips through reads.
const MAX_SELF_REF_DEPTH: u32 = 32;

fn compile_collections(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
    hosts: &HostSignatures,
) -> Result<Vec<CompiledCollection>, EngineError> {
    let mut out = Vec::new();
    for member in &schema.model().root().members {
        // §5.8: a top-level member naming a keyed `$types` shape (`companies:
        // "company"`) is a first-class collection; resolve the name before compiling.
        if let Some(collection) = schema.resolved_collection(&member.node) {
            let path = vec![member.name.as_str().to_owned()];
            out.push(compile_collection(sources, schema, root_ty, model_doc, &path, collection, hosts, 0)?);
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn compile_collection(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
    path: &[String],
    collection: &Collection,
    hosts: &HostSignatures,
    depth: u32,
) -> Result<CompiledCollection, EngineError> {
    let name = path.last().map_or("", String::as_str);
    let row_ty = schema
        .receiver_row_type(path)
        .unwrap_or_else(|| ExprType::Row(RowType::keyless(std::iter::empty())));
    let row_scope = RuntimeScope::new(row_ty.clone(), root_ty.clone()).with_host_ops(hosts.clone());

    let mut fields = Vec::new();
    let mut computed = Vec::new();
    let mut structs = Vec::new();
    let mut children = Vec::new();
    let mut views = Vec::new();
    let mut unique: Vec<Vec<String>> = collection
        .unique
        .iter()
        .map(|group| group.iter().map(|f| f.as_str().to_owned()).collect())
        .collect();

    for member in &collection.shape.members {
        let name = member.name.as_str();
        // §5.8: a member naming a `$types` shape or a `$like` positional shape is a
        // `Node::Named` that ADOPTS a keyed collection (`subcompanies: "company"`,
        // `children: { $like: "^" }`). Resolve it and compile it as an ordinary
        // nested keyed collection (§5.4), so mutations and seeds can write it. A
        // named member that is NOT a collection falls through to its own node form.
        if let Node::Named(_) = &member.node
            && let Some(nested) = schema.resolved_collection(&member.node)
        {
            // A self-referential shape is expanded to `MAX_SELF_REF_DEPTH` and then
            // cut — a deeper level contributes no compiled child; reads are bounded by
            // the actual data depth instead, so a deep tree still round-trips.
            if depth < MAX_SELF_REF_DEPTH {
                let mut child_path = path.to_vec();
                child_path.push(name.to_owned());
                children.push(compile_collection(sources, schema, root_ty, model_doc, &child_path, nested, hosts, depth + 1)?);
            }
            continue;
        }
        match &member.node {
            // A static struct member (§5.3): compiled so its own field defaults,
            // normalizers, and checks run during the containing insertion (§5.1).
            Node::Struct(shape) => {
                let mut child_path = path.to_vec();
                child_path.push(name.to_owned());
                structs.push(compile_struct(sources, schema, root_ty, model_doc, &child_path, name, shape, hosts)?);
            }
            // A nested keyed collection (§5.4): compiled recursively into a child. A
            // directly-declared nested collection is finite, so its depth is kept
            // (only self-referential `Node::Named` expansion consumes the cap).
            Node::Collection(nested) => {
                let mut child_path = path.to_vec();
                child_path.push(name.to_owned());
                children.push(compile_collection(sources, schema, root_ty, model_doc, &child_path, nested, hosts, depth)?);
            }
            // A nested `$view` member (§7.1): compiled with the row as `.` so a
            // `/coll[k].view` read (e.g. a `.modules::iface` aggregation) resolves.
            // A `$modules`/`$keyring`/source-bucket placeholder is also a
            // `Node::View`, so only a genuine `$view` doc member is compiled here.
            Node::View(view) => {
                let mut child_path = path.to_vec();
                child_path.push(name.to_owned());
                if let Some(compiled) =
                    compile_nested_view(sources, root_ty, &row_ty, model_doc, &child_path, view, hosts)
                {
                    views.push(compiled);
                }
            }
            _ => {
                let member_doc =
                    doc::shape_at(model_doc, path).and_then(|shape| doc::member(shape, name));
                if let Some(field) = compile_field(
                    sources, schema, root_ty, &row_ty, &row_scope, member, &mut unique, &mut computed, member_doc, hosts,
                )? {
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
        views,
    })
}

/// Compile a genuine nested `$view` member (§7.1) into a [`CompiledView`] typed
/// with the collection row as `.`, or `None` when the `Node::View` is a
/// `$modules`/`$keyring`/source-bucket placeholder (owned by its own materializer)
/// or the view does not compile. The doc member's shape distinguishes a real
/// `$view` from a placeholder; a view that fails to type is left unmaterialized so
/// a reader faults exactly as before rather than failing the whole load.
fn compile_nested_view(
    sources: &mut SourceMap,
    root_ty: &ExprType,
    row_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
    path: &[String],
    view: &liasse_model::ViewDecl,
    hosts: &HostSignatures,
) -> Option<CompiledView> {
    let shape = doc::shape_at(model_doc, path)?;
    // Only a `$view` doc member is a nested view; a `$modules` space and the other
    // synthetic `Node::View`s carry their own member markers and are owned elsewhere.
    doc::member(shape, "$view")?;
    let scope = RuntimeScope::new(row_ty.clone(), root_ty.clone()).with_host_ops(hosts.clone());
    let (expr, _) = compile_expr(sources, &scope, "nested-view", &view.expr.text).ok()?;
    Some(CompiledView { name: path.last()?.clone(), expr })
}

/// Compile a static struct member (§5.3): its writable fields (with defaults,
/// normalizers, and checks) and its struct-level `$check`s, so a supplied struct
/// initializer resolves omitted defaults and is validated with the row (§5.1,
/// §5.10). Nested collections inside a struct remain a documented seam.
#[allow(clippy::too_many_arguments)]
fn compile_struct(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
    path: &[String],
    name: &str,
    shape: &Shape,
    hosts: &HostSignatures,
) -> Result<CompiledStruct, EngineError> {
    let row_ty = schema
        .receiver_row_type(path)
        .unwrap_or_else(|| ExprType::Row(RowType::keyless(std::iter::empty())));
    let row_scope = RuntimeScope::new(row_ty.clone(), root_ty.clone()).with_host_ops(hosts.clone());
    let mut fields = Vec::new();
    let mut unique = Vec::new();
    let mut computed = Vec::new();
    for member in &shape.members {
        if let Node::Scalar(_) | Node::Reference(_) | Node::Set(_) = &member.node {
            let member_doc =
                doc::shape_at(model_doc, path).and_then(|s| doc::member(s, member.name.as_str()));
            if let Some(field) = compile_field(
                sources, schema, root_ty, &row_ty, &row_scope, member, &mut unique, &mut computed, member_doc, hosts,
            )? {
                fields.push(field);
            }
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
    member_doc: Option<&liasse_syntax::DocValue>,
    hosts: &HostSignatures,
) -> Result<Option<CompiledField>, EngineError> {
    let name = member.name.as_str().to_owned();
    let field = match &member.node {
        // A read-only computed value is not an insertable field (§5.2). A computed
        // value is a read/replay position, so its `row_scope` stays `Pure`.
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
            // §8.8: a field default is a write-time position, so a generated host
            // function MAY run there (a pure one always may). The `row_scope`
            // already carries the resolved host signatures; opt this one into the
            // write effect policy.
            let default = match &scalar.default {
                Some(source) => {
                    let default_scope = row_scope.clone().with_host_position(HostPosition::Write);
                    Some(compile_expr(sources, &default_scope, "default", &source.text)?)
                }
                None => None,
            };
            let field_scope =
                RuntimeScope::new(ExprType::scalar(scalar.ty.clone()), root_ty.clone()).with_host_ops(hosts.clone());
            let normalize = match &scalar.normalize {
                Some(source) => Some(compile_expr(sources, &field_scope, "normalize", &source.text)?),
                None => None,
            };
            let checks = compile_checks(sources, &field_scope, "check", &scalar.checks)?;
            CompiledField {
                name,
                ty: scalar.ty.clone(),
                reference: None,
                element_reference: None,
                default,
                normalize,
                checks,
            }
        }
        Node::Reference(reference) => {
            let target = reference.target.trim_start_matches('/').to_owned();
            let on_delete = compile_on_delete(sources, schema, root_ty, row_ty, &target, reference)?;
            CompiledField {
                name,
                ty: Type::Ref(liasse_value::RefTarget::for_key(&reference.key_type)),
                reference: Some(RefInfo { target, optional: reference.optional, on_delete }),
                element_reference: None,
                default: None,
                normalize: None,
                checks: Vec::new(),
            }
        }
        // §5.5: a `$set` of `$ref` carries per-member references (§5.6). The model
        // keeps only the element key type, so the target relation is recovered from
        // the field's definition document; every member then resolves through the
        // same integrity check and atomic-rekey rewrite as a scalar ref.
        Node::Set(set) => {
            let target = matches!(set.element, Type::Ref(_))
                .then(|| set_ref_target(member_doc))
                .flatten();
            // §5.6: the model leaves a `$set` of `$ref` element target unresolved
            // (a documented seam). Recover the target's key type here so a member
            // supplied as wire decodes to the correct scalar or positional
            // composite ref (`Ref::composite`), matching a direct `$ref` field.
            let element = match &target {
                Some(name) => schema.collection_key_type(name).map_or_else(
                    || set.element.clone(),
                    |key| Type::Ref(liasse_value::RefTarget::for_key(&key)),
                ),
                None => set.element.clone(),
            };
            // §21.1/§5.6: a `$set` of `$ref` member is a governed inbound ref, so
            // compile its DECLARED `$on_delete` (kept on the model's set field)
            // through the same policy machinery as a scalar ref. Hardcoding
            // `Undecided` here would load a declared restrict/cascade/patch policy
            // but leave it inert at delete time, stranding set members at a removed
            // row (§22.1). An undeclared policy stays `Undecided` — the §21.1 static
            // gate proved the target undeletable, so no delete exercises this edge.
            let element_reference = match target {
                Some(target) => {
                    let on_delete = match &set.element_ref {
                        Some(reference) => {
                            compile_on_delete(sources, schema, root_ty, row_ty, &target, reference)?
                        }
                        None => OnDelete::Undecided,
                    };
                    let optional = set.element_ref.as_ref().is_some_and(|reference| reference.optional);
                    Some(RefInfo { target, optional, on_delete })
                }
                None => None,
            };
            CompiledField {
                name,
                ty: Type::Set(Box::new(element)),
                reference: None,
                element_reference,
                default: None,
                normalize: None,
                checks: Vec::new(),
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(field))
}

/// The target relation of a `$set` of `$ref` member (§5.5/§5.6), read from the
/// field's definition document `{ "$set": { "$ref": "/accounts" } }` because the
/// model flattens a set element to a bare key type and drops the target name. The
/// leading `/` of the absolute target path is stripped to the collection name.
fn set_ref_target(member_doc: Option<&liasse_syntax::DocValue>) -> Option<String> {
    let set = doc::member(member_doc?, "$set")?;
    let target = doc::string(doc::member(set, "$ref")?)?;
    Some(target.trim().trim_start_matches('/').to_owned())
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
    hosts: &HostSignatures,
) -> Result<Vec<CompiledMutation>, EngineError> {
    let context_structurals = auth.structurals();
    let mut out = Vec::new();
    for mutation in schema.model().mutations() {
        let receiver_ty = schema
            .receiver_row_type(&mutation.path)
            .ok_or_else(|| EngineError::Internal(format!("mutation `{}` has no receiver", mutation.name.as_str())))?;
        // §16.3/§8.8: a mutation-program value is a write-time position, so a
        // resolved generated (or pure) host call may appear in a value expression.
        let mut scope = RuntimeScope::new(receiver_ty, root_ty.clone())
            .with_host_ops(hosts.clone())
            .with_host_position(HostPosition::Write);
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
    hosts: &HostSignatures,
) -> Result<Vec<CompiledComputed>, EngineError> {
    let scope = RuntimeScope::new(root_ty.clone(), root_ty.clone()).with_host_ops(hosts.clone());
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

/// Compile the root singleton row as a pseudo-collection (§8.2): each writable
/// scalar / reference / set member declared directly under `$model` becomes a
/// [`CompiledField`] carrying its checks, its scalar-ref target, and — for a
/// `$set` of `$ref` — its element target, and each static-struct member (§5.3)
/// becomes a [`CompiledStruct`] in `structs`. The reserved `$root` row has no key,
/// so the final rule pass (`rules::finalize`) and the atomic-rekey inbound-ref
/// rewrite validate/rewrite the singleton row exactly like a keyed row (§5.4,
/// §5.6, §22.1); the compiled `structs` let the migration final check re-validate
/// a struct-nested scalar/enum leaf the same way it does for a keyed collection
/// (§20.1). Field checks type with the member's own value as `.`, so a negative
/// `count` under `$check: [(. >= 0), …]` rejects (§8.8).
fn compile_root_singleton(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
    hosts: &HostSignatures,
) -> Result<CompiledCollection, EngineError> {
    let row_scope = RuntimeScope::new(root_ty.clone(), root_ty.clone()).with_host_ops(hosts.clone());
    let mut fields = Vec::new();
    let mut structs = Vec::new();
    let mut unique = Vec::new();
    let mut computed = Vec::new();
    for member in &schema.model().root().members {
        // §5.3/§8.2: a root static-struct member is durable singleton state whose
        // own scalar/enum leaves must be re-validated on migration exactly like a
        // keyed collection's struct member. Compile it into `structs` so the
        // singleton pseudo-collection carries it — `compile_field` returns
        // `Ok(None)` for a `Node::Struct`, which used to drop it entirely, leaving
        // `root_singleton.structs` empty and the migration final check unable to
        // re-validate a struct-nested narrowed enum (§20.1/§22.1).
        if let Node::Struct(shape) = &member.node {
            let path = vec![member.name.as_str().to_owned()];
            structs.push(compile_struct(
                sources, schema, root_ty, model_doc, &path, member.name.as_str(), shape, hosts,
            )?);
            continue;
        }
        let member_doc = doc::member(model_doc, member.name.as_str());
        if let Some(field) = compile_field(
            sources, schema, root_ty, root_ty, &row_scope, member, &mut unique, &mut computed, member_doc, hosts,
        )? {
            fields.push(field);
        }
    }
    Ok(CompiledCollection {
        name: crate::singleton::ROOT_NAME.to_owned(),
        key: Vec::new(),
        unique,
        fields,
        computed: Vec::new(),
        row_checks: Vec::new(),
        structs,
        children: Vec::new(),
        views: Vec::new(),
    })
}

/// Compile the insertion default of each writable singleton root field (§8.2). A
/// default is a write-time position, so it may call generated host functions
/// (§8.8); it types over the package root (`.` = the root row).
fn compile_root_singleton_defaults(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    hosts: &HostSignatures,
) -> Result<Vec<CompiledSingletonDefault>, EngineError> {
    let scope = RuntimeScope::new(root_ty.clone(), root_ty.clone())
        .with_host_ops(hosts.clone())
        .with_host_position(HostPosition::Write);
    let mut out = Vec::new();
    for member in &schema.model().root().members {
        if let Node::Scalar(scalar) = &member.node
            && scalar.is_writable()
            && let Some(source) = &scalar.default
        {
            let (default, _src) = compile_expr(sources, &scope, "default", &source.text)?;
            out.push(CompiledSingletonDefault { name: member.name.as_str().to_owned(), default });
        }
    }
    Ok(out)
}

/// Compile the `$normalize` of each writable singleton root field (§8.2/§8.8). A
/// normalizer types with the member's own value as `.` (a scalar) over the
/// package root, exactly like a collection field's normalizer
/// ([`compile_field`]), so `.field = @in` yields the normalized committed value
/// (§8.3 "the assigned target still applies its own normalization").
fn compile_root_singleton_normalizes(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    hosts: &HostSignatures,
) -> Result<Vec<CompiledSingletonNormalize>, EngineError> {
    let mut out = Vec::new();
    for member in &schema.model().root().members {
        if let Node::Scalar(scalar) = &member.node
            && scalar.is_writable()
            && let Some(source) = &scalar.normalize
        {
            let field_scope = RuntimeScope::new(ExprType::scalar(scalar.ty.clone()), root_ty.clone())
                .with_host_ops(hosts.clone());
            let (normalize, _src) = compile_expr(sources, &field_scope, "normalize", &source.text)?;
            out.push(CompiledSingletonNormalize { name: member.name.as_str().to_owned(), normalize });
        }
    }
    Ok(out)
}

/// Walk the `$model` document for `$modules` spaces (§13.2), recording each space's
/// declaration-name path and its `$interfaces` boundary contracts. `prefix` is the
/// declaration path of the containing object; a `$modules` member is recorded, and
/// a keyed collection is descended so a row-scoped space
/// (`companies.…​.modules`) is found. Reads the document directly because the model
/// projects a `$modules` node as an opaque placeholder view.
fn compile_module_spaces(model_doc: &liasse_syntax::DocValue, prefix: &[String]) -> Vec<CompiledModuleSpace> {
    let mut out = Vec::new();
    let Some(members) = doc::object(model_doc) else {
        return out;
    };
    for member in members {
        // `$mut`/`$types`/other reserved model members are never module spaces or
        // collections; skip them so only declared shapes are walked.
        if member.name.text.starts_with('$') {
            continue;
        }
        let mut path = prefix.to_vec();
        path.push(member.name.text.clone());
        if doc::member(&member.value, "$modules").is_some() {
            out.push(CompiledModuleSpace { path, interfaces: compile_interface_contracts(&member.value) });
        } else if doc::member(&member.value, "$key").is_some() {
            out.extend(compile_module_spaces(&member.value, &path));
        }
    }
    out
}

/// The `$interfaces` boundary contracts of a `$modules` space node (§13.8): each
/// interface name and the `(field, type)` pairs its `$view` shape declares.
fn compile_interface_contracts(space_node: &liasse_syntax::DocValue) -> Vec<CompiledInterfaceContract> {
    let Some(interfaces) =
        doc::member(space_node, "$modules").and_then(|m| doc::member(m, "$interfaces")).and_then(doc::object)
    else {
        return Vec::new();
    };
    interfaces
        .iter()
        .map(|iface| CompiledInterfaceContract {
            name: iface.name.text.clone(),
            view_fields: doc::member(&iface.value, "$view").map(interface_view_fields).unwrap_or_default(),
        })
        .collect()
}

/// The `(field, type)` pairs an interface `$view` shape declares (§13.8): each
/// non-`$` member mapping a field name to its scalar type. A `$key`/`$sort`
/// directive and a field whose type is not a lowerable scalar are skipped, so the
/// contract carries exactly the typed fields structural satisfaction compares.
fn interface_view_fields(view: &liasse_syntax::DocValue) -> Vec<(String, Type)> {
    let Some(members) = doc::object(view) else {
        return Vec::new();
    };
    members
        .iter()
        .filter(|m| !m.name.text.starts_with('$'))
        .filter_map(|m| {
            let text = doc::string(&m.value)?;
            lower_scalar_type(text.trim()).map(|ty| (m.name.text.clone(), ty))
        })
        .collect()
}

fn compile_views(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    keyrings: &[CompiledKeyring],
    model_doc: &liasse_syntax::DocValue,
    hosts: &HostSignatures,
) -> Result<Vec<CompiledView>, EngineError> {
    let scope = RuntimeScope::new(root_ty.clone(), root_ty.clone()).with_host_ops(hosts.clone());
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
            // §13.2: a `$modules` space is projected as a `Node::View` for typing,
            // but its rows are the installed instances the module host folds in, not
            // its `.` placeholder — compiling it would overwrite the injected spaces
            // with a whole-root clone. The module aggregation owns the member.
            if doc::member(model_doc, name).is_some_and(|value| doc::member(value, "$modules").is_some()) {
                continue;
            }
            let (expr, _source) = compile_expr(sources, &scope, "view", &view.expr.text)?;
            out.push(CompiledView { name: name.to_owned(), expr });
        }
    }
    Ok(out)
}

/// Compile each `$expose` interface's `$view` (§13.8) into a typed projection over
/// the module root, so an interface-addressed read (§13.9) evaluates it against a
/// child instance. The model's expose phase already typed each `$view` against the
/// same root scope; re-checking it here in the runtime scope produces the
/// evaluable [`TypedExpr`] the boundary read uses. An interface that binds only
/// mutations (no `$view`) contributes nothing readable.
fn compile_exposed_views(
    sources: &mut SourceMap,
    root_ty: &ExprType,
    model: &Model,
    hosts: &HostSignatures,
) -> Result<Vec<CompiledExposed>, EngineError> {
    let scope = RuntimeScope::new(root_ty.clone(), root_ty.clone()).with_host_ops(hosts.clone());
    let mut out = Vec::new();
    for interface in model.exposed_interfaces() {
        let Some(view) = interface.view.as_ref() else { continue };
        let (expr, _source) = compile_expr(sources, &scope, "expose-view", &view.text)?;
        out.push(CompiledExposed { interface: interface.name.as_str().to_owned(), expr });
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

/// Enforce the §17.1 keyring `$usage` rule at load (§9.2 step 5). Every keyring a
/// mutation call site signs with (`cose.sign(/ring, …)`) requires the protected
/// `sign` operation. A declared `$usage` MUST include every protected operation a
/// call site performs; a declared `$usage` that excludes a required operation is a
/// load rejection, so `$usage: []` on a signed ring rejects. An omitted `$usage`
/// is inferred to the required set. Verification is a public operation and is
/// never required in `$usage`, so verifier call sites are not consulted here.
pub(crate) fn enforce_keyring_usage(
    mutations: &[CompiledMutation],
    keyrings: &mut [CompiledKeyring],
    model_doc: &liasse_syntax::DocValue,
    src: SourceId,
) -> Result<(), EngineError> {
    if keyrings.is_empty() {
        return Ok(());
    }
    let signed = signed_rings(mutations);
    for keyring in keyrings {
        if !signed.contains(&keyring.name) {
            continue;
        }
        // The one protected operation a mutation call site infers today is `sign`.
        let required = KeyOperation::Sign;
        match declared_usage_span(model_doc, &keyring.name) {
            Some(usage_span) => {
                if !keyring.policy.usage.contains(&required) {
                    return Err(keyring_usage_rejection(&keyring.name, src, usage_span));
                }
            }
            // §17.1: an omitted `$usage` is the inferred minimal operation set.
            None => {
                keyring.policy.usage.insert(required);
            }
        }
    }
    Ok(())
}

/// The declared keyrings a mutation program signs with, gathered from every
/// `X.sign(/ring, …)` call site anywhere in a mutation statement (§17.7).
fn signed_rings(mutations: &[CompiledMutation]) -> BTreeSet<String> {
    let mut signed = BTreeSet::new();
    for mutation in mutations {
        for statement in &mutation.program {
            walk_stmt(&statement.stmt, &mut signed);
        }
    }
    signed
}

fn walk_stmt(stmt: &Stmt, signed: &mut BTreeSet<String>) {
    match &stmt.kind {
        StmtKind::Return(expr) | StmtKind::Clear(expr) | StmtKind::Bare(expr) => walk_expr(expr, signed),
        StmtKind::Assign { target, value } => {
            walk_expr(target, signed);
            walk_expr(value, signed);
        }
    }
}

fn walk_expr(expr: &Expr, signed: &mut BTreeSet<String>) {
    if let Some(ring) = signed_ring_of(expr) {
        signed.insert(ring.to_owned());
    }
    walk_children(expr, signed);
}

/// The keyring name `expr` signs with when it is exactly a `X.sign(/ring, …)`
/// call whose first argument is a `/ring` root path (§17.7).
fn signed_ring_of(expr: &Expr) -> Option<&str> {
    let ExprKind::Call { callee, args } = &expr.kind else { return None };
    let ExprKind::Field { member, .. } = &callee.kind else { return None };
    if member.structural || member.text != "sign" {
        return None;
    }
    keyring_path(arg_value(args.first()?))
}

fn arg_value(arg: &Arg) -> &Expr {
    match arg {
        Arg::Positional(value) | Arg::Named { value, .. } => value,
    }
}

/// The ring name a `/ring` root-field path addresses (§17.7): `Field { base:
/// Root, member }` with a non-structural member.
fn keyring_path(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Field { base, member } if matches!(base.kind, ExprKind::Root) && !member.structural => {
            Some(&member.text)
        }
        _ => None,
    }
}

fn walk_children(expr: &Expr, signed: &mut BTreeSet<String>) {
    match &expr.kind {
        ExprKind::None
        | ExprKind::Bool(_)
        | ExprKind::Int(_)
        | ExprKind::Decimal(_)
        | ExprKind::Str(_)
        | ExprKind::Root
        | ExprKind::Current
        | ExprKind::Parent(_)
        | ExprKind::Import(_)
        | ExprKind::Param(_)
        | ExprKind::Structural(_)
        | ExprKind::Name(_) => {}
        ExprKind::List(items) => items.iter().for_each(|item| walk_expr(item, signed)),
        ExprKind::Object(members) => members.iter().for_each(|member| walk_member(member, signed)),
        ExprKind::Field { base, .. } | ExprKind::SameName { base, .. } => walk_expr(base, signed),
        ExprKind::Select { base, selector } => {
            walk_expr(base, signed);
            walk_selector(selector, signed);
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, signed);
            args.iter().for_each(|arg| walk_expr(arg_value(arg), signed));
        }
        ExprKind::Block { base, members } => {
            walk_expr(base, signed);
            members.iter().for_each(|member| walk_member(member, signed));
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand, signed),
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, signed);
            walk_expr(rhs, signed);
        }
        ExprKind::Ternary { cond, then, otherwise } => {
            walk_expr(cond, signed);
            walk_expr(then, signed);
            walk_expr(otherwise, signed);
        }
        ExprKind::Combination { operands, .. } => operands.iter().for_each(|operand| walk_expr(operand, signed)),
    }
}

fn walk_selector(selector: &Selector, signed: &mut BTreeSet<String>) {
    match selector {
        Selector::Keys(keys) => keys.iter().for_each(|key| walk_expr(key, signed)),
        Selector::Bind { condition, .. } => {
            if let Some(condition) = condition {
                walk_expr(condition, signed);
            }
        }
    }
}

fn walk_member(member: &BlockMember, signed: &mut BTreeSet<String>) {
    match &member.kind {
        BlockMemberKind::Directive { value, .. } | BlockMemberKind::Assign { value, .. } => {
            walk_expr(value, signed);
        }
        BlockMemberKind::Named { value: Some(value), .. } | BlockMemberKind::Shorthand(value) => {
            walk_expr(value, signed);
        }
        BlockMemberKind::Named { value: None, .. } | BlockMemberKind::Clear(_) => {}
    }
}

/// The byte span of a keyring's declared `$usage` member, or `None` when it is
/// omitted (§17.1 inference applies).
fn declared_usage_span(model_doc: &liasse_syntax::DocValue, name: &str) -> Option<liasse_diag::ByteSpan> {
    let path = [name.to_owned()];
    let shape = doc::shape_at(model_doc, &path)?;
    let keyring = doc::member(shape, "$keyring")?;
    doc::member(keyring, "$usage").map(|usage| usage.span)
}

/// The §17.1 load rejection for a declared `$usage` that excludes a required
/// protected operation.
fn keyring_usage_rejection(name: &str, src: SourceId, span: liasse_diag::ByteSpan) -> EngineError {
    let mut diagnostics = Diagnostics::new();
    diagnostics.push(
        Diagnostic::error(format!(
            "keyring `{name}` declares a `$usage` that excludes the `sign` operation a call site requires"
        ))
        .code("keyring")
        .primary(Span::new(src, span), "this `$usage` permits no signing")
        .help("add `sign` to `$usage`, or omit `$usage` to infer it from the call sites (§17.1)")
        .build(),
    );
    EngineError::Invalid(Box::new(diagnostics))
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
    hosts: &HostSignatures,
) -> Vec<CompiledSurfaceView> {
    let mut out = Vec::new();
    let structurals = auth.structurals();
    if let Some(public) = doc::member(model_doc, "$public").and_then(doc::object) {
        for surface in public {
            compile_one_surface_view(
                sources,
                root_ty,
                None,
                &format!("public.{}", surface.name.text),
                &surface.value,
                &structurals,
                hosts,
                &mut out,
            );
        }
    }
    // A package-level `$roles` block: its surfaces read `.` as the package root
    // (an unscoped role), so they compile against `root_ty` like a `$public` one.
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
                    None,
                    &format!("{}.{}", role.name.text, member.name.text),
                    &member.value,
                    &structurals,
                    hosts,
                    &mut out,
                );
            }
        }
    }
    // §10.3/§10.5: a role nested on a collection row is a SCOPED role — its
    // surface `$view` (and `$recursive` coverage) reads `.` as the role-holding
    // row, keyed by the request scope. Compile each against that collection's row
    // type, recording the scope path so the read resolves the covered row.
    for collection in doc::object(model_doc).into_iter().flatten() {
        if collection.name.text.starts_with('$') {
            continue;
        }
        let Some(roles) = doc::member(&collection.value, "$roles").and_then(doc::object) else {
            continue;
        };
        let path = vec![collection.name.text.clone()];
        let Some(covered_ty) = schema.receiver_row_type(&path) else { continue };
        for role in roles {
            let Some(members) = doc::object(&role.value) else { continue };
            for member in members {
                if member.name.text.starts_with('$') {
                    continue;
                }
                compile_one_surface_view(
                    sources,
                    root_ty,
                    Some((&path, &covered_ty)),
                    &format!("{}.{}", role.name.text, member.name.text),
                    &member.value,
                    &structurals,
                    hosts,
                    &mut out,
                );
            }
        }
    }
    out
}

/// Compile one surface declaration's `$view` (§10.1) at dotted `address`, adding
/// it to `out` when it carries a compilable `$view`. A surface with only `$mut`
/// calls contributes nothing here.
///
/// `covered` is `Some((path, row_ty))` for a scoped-role surface (§10.3/§10.5):
/// its `$view`/`$recursive` read `.` as the role-holding row at `path`, so they
/// compile against `row_ty`, and the recorded [`CompiledScope`] resolves that row
/// from the request scope at read time. `None` for a `$public`/package-level view,
/// whose `.` is the package root.
#[allow(clippy::too_many_arguments)]
fn compile_one_surface_view(
    sources: &mut SourceMap,
    root_ty: &ExprType,
    covered: Option<(&[String], &ExprType)>,
    address: &str,
    surface: &liasse_syntax::DocValue,
    structurals: &[(String, ExprType)],
    hosts: &HostSignatures,
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
    let current_ty = covered.map_or(root_ty, |(_, row_ty)| row_ty);
    let mut scope = RuntimeScope::new(current_ty.clone(), root_ty.clone()).with_host_ops(hosts.clone());
    for param in &params {
        scope = scope.with_param(param.name.clone(), param.ty.clone());
    }
    for (name, ty) in structurals {
        scope = scope.with_structural(name.clone(), ty.clone());
    }
    let Ok((expr, _)) = compile_expr(sources, &scope, "surface-view", view_text) else {
        return;
    };
    let scope_binding = match covered {
        None => None,
        Some((path, covered_ty)) => {
            // §10.5: compile the `$recursive` coverage when declared. If it is
            // declared but does not compile (unreachable for a package the model
            // validated), drop the whole surface rather than serve it uncovered.
            let declared = doc::member(surface, "$recursive").is_some();
            let recursive =
                compile_recursive(sources, covered_ty, root_ty, surface, &params, structurals, hosts);
            if declared && recursive.is_none() {
                return;
            }
            Some(CompiledScope { collection_path: path.to_vec(), recursive })
        }
    };
    out.push(CompiledSurfaceView { address: address.to_owned(), expr, params, scope: scope_binding });
}

/// Compile a scoped-role surface's `$recursive` coverage block (§10.5), or `None`
/// when the surface declares none (or a declared predicate does not compile). The
/// `$field`/`$bind` name the descendant relation and candidate; `$where`/`$except`
/// are hereditary `bool` predicates that read the candidate through `$bind`, so
/// they compile against the covered row with the candidate row bound to `$bind`.
fn compile_recursive(
    sources: &mut SourceMap,
    covered_ty: &ExprType,
    root_ty: &ExprType,
    surface: &liasse_syntax::DocValue,
    params: &[CompiledParam],
    structurals: &[(String, ExprType)],
    hosts: &HostSignatures,
) -> Option<CompiledRecursive> {
    let recursive = doc::member(surface, "$recursive")?;
    let field = doc::member(recursive, "$field").and_then(doc::string)?.trim().to_owned();
    let bind = doc::member(recursive, "$bind").and_then(doc::string)?.trim().to_owned();
    let candidate = field_row_type(covered_ty, &field)?;
    let mut where_pred = None;
    let mut except_pred = None;
    for (directive, slot) in [("$where", &mut where_pred), ("$except", &mut except_pred)] {
        let Some(text) = doc::member(recursive, directive).and_then(doc::string) else { continue };
        let mut scope = RuntimeScope::new(covered_ty.clone(), root_ty.clone())
            .with_host_ops(hosts.clone())
            .with_binding(bind.clone(), candidate.clone());
        for param in params {
            scope = scope.with_param(param.name.clone(), param.ty.clone());
        }
        for (name, ty) in structurals {
            scope = scope.with_structural(name.clone(), ty.clone());
        }
        let (expr, _) = compile_expr(sources, &scope, "recursive-predicate", text.trim()).ok()?;
        *slot = Some(expr);
    }
    Some(CompiledRecursive { field, bind, where_pred, except_pred })
}

/// The single-row type of a keyed-collection field of `covered` (§10.5): the
/// candidate a `$recursive` `$bind` names. `None` when the field is not a keyed
/// collection (the model rejects such a `$field`, so this is a compile guard).
fn field_row_type(covered: &ExprType, field: &str) -> Option<ExprType> {
    match covered.as_row()?.field(field)? {
        ExprType::View(row) | ExprType::Row(row) => Some(ExprType::Row(row.clone())),
        _ => None,
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
