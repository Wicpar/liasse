//! Annex E boundary-contract narrowing check for package update (§20.3, §13.14).
//!
//! Within one package major, a minor or patch release MUST preserve or widen the
//! exposed boundary contract; `load` and update reject a narrowing release before
//! activation (E.1, E.9). [`CompatibilityDecision`](liasse_artifact::CompatibilityDecision)
//! decides *which* rule the version relationship carries; this module runs the
//! rule over the effective contracts once the relationship is a same-line forward
//! move (a minor or patch).
//!
//! A [`BoundaryContract`] is the observable promise an independently versioned
//! client relies on (E.2): each `$public`/role surface's view output shape,
//! identity, and explicit ordering, its view parameters, and each `$mut`-bound
//! operation's accepted input and promised response. [`BoundaryContract::narrowing`]
//! compares a candidate against the active contract and reports the first
//! narrowing it can establish — a removed surface or operation, a removed or
//! type-narrowed output *or response* member, an enum result whose exhaustive
//! domain changed, a
//! changed explicit view ordering (E.5 "changing explicit sort semantics"), a
//! required parameter added, or an accepted input domain narrowed (E.4, E.5, E.7).
//!
//! Explicit ordering (E.2/E.5): a `$public`/role view's `$sort` is a boundary
//! contract, so a same-major forward release MUST preserve it. The contract reads
//! the view's top-level projection `$sort` — normalized to an ordered
//! `(key, descending)` sequence so the three §7.3 spellings (`"-name"`, `-name`,
//! `{ $by: name, $dir: desc }`) compare equal — and rejects any change (a
//! direction flip, or a key added, removed, replaced, or reordered) as a
//! narrowing. An unchanged `$sort` compares equal and passes; a major release
//! bypasses the whole check (E.1).
//!
//! Boundaries this CORE check does not yet compare — module-interface bindings and
//! host-capability requirements (E.6/E.8), a mutation response that is not a plain
//! projection, and a view over a nested or combinator source (its output shape and
//! its ordering alike) — are left unconstrained rather than mis-flagged, so the
//! check never rejects a compatible release and defers those classes as documented
//! seams.

use std::collections::BTreeMap;

use liasse_diag::SourceId;
use liasse_expr::{check_expression, ExprType};
use liasse_syntax::{BinaryOp, DocValue, Expr, ExprKind, StmtKind};
use liasse_value::Type;

use crate::compiled::{Compiled, CompiledMutation, CompiledSurfaceView};
use crate::doc;
use crate::scope::RuntimeScope;

mod ordering;
use ordering::view_ordering;

/// One parameter's boundary contract (E.4): its accepted type and whether a
/// caller must supply it. A parameter is required when it is neither optional nor
/// defaulted, so omitting it under the earlier contract must stay valid.
struct Param {
    ty: ExprType,
    required: bool,
}

/// A surface view's exposed output (E.5): the projected members with their types
/// and the exposed row identity, when it derives from a plain collection source.
struct Output {
    members: BTreeMap<String, ExprType>,
    /// The exposed row identity (A.9/E.5): the collection's `$key` components in
    /// `$key` order, each `(name, type)`. A key is a *typed tuple* (A.9), so the
    /// identity carries each component's type — a component retype (composite or
    /// scalar) with unchanged names is still an identity change E.3 mechanically
    /// compares and E.5 forbids. `None` when the source is not a plain top-level
    /// collection.
    identity: Option<Vec<(String, Type)>>,
    /// The exposed explicit ordering (E.2/E.5): the ordered `(key, descending)`
    /// sequence the view's top-level projection `$sort` declares, normalized so
    /// the §7.3 string/compact/structured spellings compare equal — empty when the
    /// projection declares no `$sort`. `None` when the `$view` is not a plain
    /// top-level projection whose ordering the check can read (a combinator or
    /// nested source is a documented seam, left uncompared to avoid over-rejection).
    ordering: Option<Vec<(String, bool)>>,
}

/// One exposed operation bound through a surface `$mut` (E.7): its accepted input
/// and the response members it promises, each typed like a view output member so
/// the identical narrowing rule applies (E.5/E.7). `response` is `None` when the
/// mutation's `return` is not a plain projection the check can type.
struct Operation {
    params: BTreeMap<String, Param>,
    response: Option<BTreeMap<String, ExprType>>,
}

/// One boundary surface (§10.1): its view output/parameters and exposed
/// operations. A surface with only `$mut` operations carries no `output`.
struct Surface {
    output: Option<Output>,
    params: BTreeMap<String, Param>,
    operations: BTreeMap<String, Operation>,
}

/// The effective boundary contract of one release (Annex E.2): every
/// externally addressable surface keyed by its dotted address (`public.<name>`,
/// `<role>.<name>`).
pub(crate) struct BoundaryContract {
    surfaces: BTreeMap<String, Surface>,
}

impl BoundaryContract {
    /// Extract the effective boundary contract of a release from its compiled
    /// artefacts and `$model` document (E.2). The compiled surface views carry the
    /// typed output shapes and parameter contracts; the document carries the
    /// `$mut` bindings and view sources the compiled form does not retain.
    pub(crate) fn extract(compiled: &Compiled, model_doc: &DocValue) -> Self {
        let mut surfaces = BTreeMap::new();
        if let Some(public) = doc::member(model_doc, "$public").and_then(doc::object) {
            for surface in public {
                let address = format!("public.{}", surface.name.text);
                let contract = surface_contract(&address, &surface.value, compiled);
                surfaces.insert(address, contract);
            }
        }
        if let Some(roles) = doc::member(model_doc, "$roles").and_then(doc::object) {
            for role in roles {
                let Some(members) = doc::object(&role.value) else { continue };
                for member in members {
                    // A role's `$`-members (`$members`/`$auth`/...) are not surfaces.
                    if member.name.text.starts_with('$') {
                        continue;
                    }
                    let address = format!("{}.{}", role.name.text, member.name.text);
                    let contract = surface_contract(&address, &member.value, compiled);
                    surfaces.insert(address, contract);
                }
            }
        }
        Self { surfaces }
    }

    /// The first narrowing this `candidate` makes relative to `self` (the active
    /// boundary contract), or `None` when the candidate preserves or widens every
    /// observable contract (E.4, E.5, E.7). The comparison is one-directional:
    /// every promise the active contract makes must still hold; additive surfaces,
    /// operations, output members, and optional parameters are compatible.
    pub(crate) fn narrowing(&self, candidate: &Self) -> Option<String> {
        for (address, active) in &self.surfaces {
            let Some(cand) = candidate.surfaces.get(address) else {
                return Some(format!("surface `{address}` is removed"));
            };
            if let Some(reason) = surface_narrowing(address, active, cand) {
                return Some(reason);
            }
        }
        None
    }
}

/// Build one surface's contract: its typed view output/parameters (from the
/// compiled surface view, when the surface declares a compilable `$view`) and its
/// exposed `$mut` operations (from the document binding).
fn surface_contract(address: &str, decl: &DocValue, compiled: &Compiled) -> Surface {
    let view = compiled.surface_view(address);
    let output = view.and_then(|view| output_shape(view, decl, compiled));
    let params = view.map(view_params).unwrap_or_default();
    let operations = surface_operations(decl, compiled);
    Surface { output, params, operations }
}

/// The exposed output shape of a surface view (E.5): its projected members typed
/// from the compiled view expression, and the exposed row identity read from the
/// view's collection source.
fn output_shape(view: &CompiledSurfaceView, decl: &DocValue, compiled: &Compiled) -> Option<Output> {
    let row = view.expr.ty().as_view().or_else(|| view.expr.ty().as_row())?;
    let members = row.fields().map(|(name, ty)| (name.clone(), ty.clone())).collect();
    let identity = view_source_collection(decl).and_then(|name| exposed_identity(&name, compiled));
    let ordering = view_ordering(decl);
    Some(Output { members, identity, ordering })
}

/// The declared parameters of a surface view (§10.1) as boundary contracts.
fn view_params(view: &CompiledSurfaceView) -> BTreeMap<String, Param> {
    view.params
        .iter()
        .map(|param| {
            let required = !is_optional(&param.ty) && param.default.is_none();
            (param.name.clone(), Param { ty: param.ty.clone(), required })
        })
        .collect()
}

/// The exposed operations a surface's `$mut` block binds (§10.1, E.7), each
/// resolved to the mutation it names so its accepted input and promised response
/// become the boundary contract.
fn surface_operations(decl: &DocValue, compiled: &Compiled) -> BTreeMap<String, Operation> {
    let mut operations = BTreeMap::new();
    let Some(muts) = doc::member(decl, "$mut").and_then(doc::object) else {
        return operations;
    };
    for entry in muts {
        let Some(binding) = doc::string(&entry.value) else { continue };
        let Some(name) = bound_mutation_name(binding) else { continue };
        let Some(mutation) = compiled.mutation(&name) else { continue };
        let params = mutation
            .params
            .iter()
            .map(|(name, ty)| (name.clone(), Param { ty: ty.clone(), required: !is_optional(ty) }))
            .collect();
        let response = response_shape(mutation);
        operations.insert(entry.name.text.clone(), Operation { params, response });
    }
    operations
}

/// The first narrowing `cand` makes to the surface `active` promises (E.4/E.5/E.7).
fn surface_narrowing(address: &str, active: &Surface, cand: &Surface) -> Option<String> {
    if let Some(active_out) = &active.output {
        let Some(cand_out) = &cand.output else {
            return Some(format!("surface `{address}` no longer exposes its view output"));
        };
        if let (Some(a), Some(c)) = (&active_out.identity, &cand_out.identity)
            && let Some(reason) = identity_narrowing(address, a, c)
        {
            return Some(reason);
        }
        // E.5: "changing explicit sort semantics" is a breaking output change. The
        // exposed view's declared `$sort` ordering must be preserved on a same-major
        // forward release; a flip, added/removed/replaced/reordered key alters the
        // promised row order and narrows the ordering contract.
        if let (Some(a), Some(c)) = (&active_out.ordering, &cand_out.ordering)
            && a != c
        {
            return Some(format!(
                "surface `{address}` changes the explicit view ordering from {a:?} to {c:?}"
            ));
        }
        if let Some(reason) = projection_narrowing(
            &format!("surface `{address}`"),
            "output member",
            &active_out.members,
            &cand_out.members,
        ) {
            return Some(reason);
        }
    }
    if let Some(reason) = params_narrowing(&format!("surface `{address}` view"), &active.params, &cand.params) {
        return Some(reason);
    }
    for (name, active_op) in &active.operations {
        let Some(cand_op) = cand.operations.get(name) else {
            return Some(format!("surface `{address}` removes exposed operation `{name}`"));
        };
        let what = format!("surface `{address}` operation `{name}`");
        if let Some(reason) = params_narrowing(&what, &active_op.params, &cand_op.params) {
            return Some(reason);
        }
        if let (Some(active_resp), Some(cand_resp)) = (&active_op.response, &cand_op.response)
            && let Some(reason) = projection_narrowing(&what, "response member", active_resp, cand_resp)
        {
            return Some(reason);
        }
    }
    None
}

/// The first way `cand` narrows the exposed row identity `active` promises
/// (A.9/E.3/E.5). The identity is a *typed tuple*: its `$key` component names in
/// order **and** each component's type. Identity is invariant across a same-major
/// forward release — E.5 makes any change to the "exposed row identity" breaking —
/// so a renamed, reordered, added, or removed component narrows it, and so does a
/// retyped component. A component retype (composite OR scalar) is judged by the
/// same [`output_narrows`] the projected-output path uses, so a `text`→`int` key
/// component is caught here exactly as it would be as a projected output member
/// (E.3: "types … are what the mechanical comparison is made of").
fn identity_narrowing(address: &str, active: &[(String, Type)], cand: &[(String, Type)]) -> Option<String> {
    let names = |identity: &[(String, Type)]| identity.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>();
    let (active_names, cand_names) = (names(active), names(cand));
    if active_names != cand_names {
        return Some(format!(
            "surface `{address}` changes exposed row identity from {active_names:?} to {cand_names:?}"
        ));
    }
    for ((name, active_ty), (_, cand_ty)) in active.iter().zip(cand) {
        if output_narrows(&ExprType::scalar(active_ty.clone()), &ExprType::scalar(cand_ty.clone())) {
            return Some(format!(
                "surface `{address}` changes exposed row identity: component `{name}` retyped from `{}` to `{}`",
                active_ty.name(),
                cand_ty.name()
            ));
        }
    }
    None
}

/// The first way a candidate projection narrows the one an earlier release
/// promised (E.5): a member the candidate no longer projects, or a member whose
/// type narrows under [`output_narrows`] (a required member made optional, an
/// exhaustive enum result whose domain changed, or a changed value type). A view's
/// exposed output and a mutation's exposed response both compare through this one
/// helper so the identical rule governs both (E.3/E.5/E.7). `subject` names the
/// boundary and `noun` the member kind (`output member` / `response member`) for
/// the diagnostic. Members the candidate *adds* are additive and never narrow.
fn projection_narrowing(
    subject: &str,
    noun: &str,
    active: &BTreeMap<String, ExprType>,
    cand: &BTreeMap<String, ExprType>,
) -> Option<String> {
    for (member, active_ty) in active {
        let Some(cand_ty) = cand.get(member) else {
            return Some(format!("{subject} removes {noun} `{member}`"));
        };
        if output_narrows(active_ty, cand_ty) {
            return Some(format!("{subject} narrows {noun} `{member}`"));
        }
    }
    None
}

/// The first input narrowing between two parameter sets (E.4): a newly required
/// parameter, a parameter made required, or an existing parameter's accepted
/// domain narrowed. Removing a parameter is left unpinned (SPEC-ISSUES item 6).
fn params_narrowing(what: &str, active: &BTreeMap<String, Param>, cand: &BTreeMap<String, Param>) -> Option<String> {
    for (name, cand_param) in cand {
        if !active.contains_key(name) && cand_param.required {
            return Some(format!("{what} adds required parameter `{name}`"));
        }
    }
    for (name, active_param) in active {
        let Some(cand_param) = cand.get(name) else { continue };
        if !active_param.required && cand_param.required {
            return Some(format!("{what} makes parameter `{name}` required"));
        }
        if input_narrows(&active_param.ty, &cand_param.ty) {
            return Some(format!("{what} narrows the accepted domain of parameter `{name}`"));
        }
    }
    None
}

/// Whether `cand` narrows the active output member type (E.5): making a required
/// member optional, changing an exhaustive enum result's domain (removing *or*
/// widening a label is breaking), or changing the value type.
fn output_narrows(active: &ExprType, cand: &ExprType) -> bool {
    let (Some(active_ty), Some(cand_ty)) = (active.as_scalar(), cand.as_scalar()) else {
        return active != cand;
    };
    let (active_inner, active_opt) = strip_optional(active_ty);
    let (cand_inner, cand_opt) = strip_optional(cand_ty);
    if !active_opt && cand_opt {
        return true;
    }
    match (enum_labels(active_inner), enum_labels(cand_inner)) {
        (Some(a), Some(c)) => a != c,
        (None, None) => active_inner != cand_inner,
        _ => true,
    }
}

/// Whether `cand` narrows the active input parameter type (E.4): an accepted enum
/// domain that loses a label, or a changed value type. Adding enum labels widens
/// the accepted domain and is compatible.
fn input_narrows(active: &ExprType, cand: &ExprType) -> bool {
    let (Some(active_ty), Some(cand_ty)) = (active.as_scalar(), cand.as_scalar()) else {
        return active != cand;
    };
    let (active_inner, _) = strip_optional(active_ty);
    let (cand_inner, _) = strip_optional(cand_ty);
    match (enum_labels(active_inner), enum_labels(cand_inner)) {
        (Some(a), Some(c)) => !a.iter().all(|label| c.contains(label)),
        (None, None) => active_inner != cand_inner,
        _ => true,
    }
}

/// The `(inner, is_optional)` of a scalar type, peeling one `optional<T>` layer.
fn strip_optional(ty: &Type) -> (&Type, bool) {
    match ty {
        Type::Optional(inner) => (inner.as_ref(), true),
        other => (other, false),
    }
}

/// The declared labels of an enum type, or `None` for a non-enum type.
fn enum_labels(ty: &Type) -> Option<&[String]> {
    match ty {
        Type::Enum(enumeration) => Some(enumeration.labels()),
        _ => None,
    }
}

/// Whether an expression result type is an `optional<T>` scalar.
fn is_optional(ty: &ExprType) -> bool {
    matches!(ty.as_scalar(), Some(Type::Optional(_)))
}

/// The mutation a surface `$mut` binding names (§10.1): the final `.name` segment
/// of a bare (`.add_task`) or row-scoped (`.companies[@id].rename()`) reference.
/// `None` when the binding is not a resolvable mutation reference.
fn bound_mutation_name(binding: &str) -> Option<String> {
    let text = binding.trim();
    let text = text.strip_prefix('=').map_or(text, str::trim);
    let text = text.strip_suffix("()").unwrap_or(text);
    let tail = text.rsplit('.').next()?.trim();
    if tail.is_empty() || !tail.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some(tail.to_owned())
}

/// The typed response members a mutation's `return` projection promises
/// (E.5/E.7): each projected member with its inferred type, obtained by typing the
/// trailing `return` projection against the mutation's scope — the same typed
/// shape the view-output path reads, so [`projection_narrowing`] governs both
/// identically. The program's `name = …` locals are threaded into that scope (see
/// [`local_type`]), so a `return t { … }` over a constructed row types as well as
/// a `return . { … }` over the receiver. `None` when the trailing `return` is not
/// a plain projection or does not type, so an opaque response is never mistaken
/// for a narrowing (the documented seam).
fn response_shape(mutation: &CompiledMutation) -> Option<BTreeMap<String, ExprType>> {
    let mut scope = mutation.scope.clone();
    let mut response = None;
    for stmt in &mutation.program {
        match &stmt.stmt.kind {
            // A `name = …` statement binds a lexical local (§8.1); thread its type
            // so a later `return` projecting that local can be typed. A `.field = …`
            // / `.coll = …` state write has a non-`Name` target and binds nothing.
            StmtKind::Assign { target, value } => {
                if let ExprKind::Name(local) = &target.kind
                    && let Some(ty) = local_type(value, &scope, stmt.source)
                {
                    scope = scope.with_binding(local.text.clone(), ty);
                }
            }
            StmtKind::Return(expr) => response = Some((expr, stmt.source)),
            StmtKind::Bare(_) | StmtKind::Clear(_) => {}
        }
    }
    let (expr, source) = response?;
    // Seam: only a plain projection response carries a comparable member shape; a
    // scalar/ref/bare-row return stays opaque and uncompared.
    if !matches!(expr.kind, ExprKind::Block { .. }) {
        return None;
    }
    let typed = check_expression(&scope, source, expr).ok()?;
    let row = typed.ty().as_view().or_else(|| typed.ty().as_row())?;
    Some(row.fields().map(|(name, ty)| (name.clone(), ty.clone())).collect())
}

/// The type a mutation local `name = value` binds (§8.1), for typing a `return`
/// projection that reads a constructed or selected row. An insert `.coll + { … }`
/// binds the inserted row (the collection's row type, read from the collection
/// reference on the left); any other value binds the type it checks to. `None`
/// when the value does not type against `scope` on its own (a delete form the
/// expression checker does not type is left unbound — the response then stays an
/// uncompared seam rather than a mis-typed contract).
fn local_type(value: &Expr, scope: &RuntimeScope, source: SourceId) -> Option<ExprType> {
    if let ExprKind::Binary { op: BinaryOp::Add, lhs, .. } = &value.kind
        && let Ok(base) = check_expression(scope, source, lhs)
        && let Some(row) = base.ty().as_view()
    {
        return Some(ExprType::Row(row.clone()));
    }
    check_expression(scope, source, value).ok().map(|typed| typed.ty().clone())
}

/// The exposed row identity of a surface view (A.9/E.5): the top-level
/// collection's `$key` components in `$key` order, each `(name, type)`. The key is
/// a typed tuple (A.9), so each component carries its declared field type. A scalar
/// key component resolves through the collection's writable `field`; a struct-typed
/// key component (A.8: "structs composed solely of key-eligible required fields")
/// compiles into the collection's `structs`, not its `fields`, so it resolves
/// through `struct_type` to the same field-name-ordered `Type::Struct` the schema's
/// key builder produces (§5.4). Only a component that names neither — a genuinely
/// unresolvable stale declaration — falls back to `json`. `None` when the source is
/// not a plain top-level collection.
fn exposed_identity(collection: &str, compiled: &Compiled) -> Option<Vec<(String, Type)>> {
    let collection = compiled.collection(collection)?;
    let identity = collection
        .key
        .iter()
        .map(|name| {
            let ty = collection
                .field(name)
                .map(|field| field.ty.clone())
                .or_else(|| collection.struct_type(name))
                .unwrap_or(Type::Json);
            (name.clone(), ty)
        })
        .collect();
    Some(identity)
}

/// The top-level collection a view text projects, taken from its leading
/// `.<collection>` source (`.companies { ... }` → `companies`). `None` when the
/// text does not begin with a plain collection reference.
fn view_source_collection(decl: &DocValue) -> Option<String> {
    let text = doc::member(decl, "$view").and_then(doc::string)?;
    let rest = text.trim_start().strip_prefix('.')?;
    let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(rest.len());
    let name = rest.get(..end)?;
    (!name.is_empty()).then(|| name.to_owned())
}

