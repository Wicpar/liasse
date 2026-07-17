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
//! type-narrowed output member, an enum result whose exhaustive domain changed, a
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

use std::collections::{BTreeMap, BTreeSet};

use liasse_diag::SourceMap;
use liasse_expr::ExprType;
use liasse_syntax::{
    parse_expression, BlockMember, BlockMemberKind, DocValue, Expr, ExprKind, StmtKind, UnaryOp,
};
use liasse_value::Type;

use crate::compiled::{Compiled, CompiledMutation, CompiledSurfaceView};
use crate::doc;

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
    /// The `$key` fields of the collection the view projects — the exposed row
    /// identity (E.5). `None` when the source is not a plain top-level collection.
    identity: Option<Vec<String>>,
    /// The exposed explicit ordering (E.2/E.5): the ordered `(key, descending)`
    /// sequence the view's top-level projection `$sort` declares, normalized so
    /// the §7.3 string/compact/structured spellings compare equal — empty when the
    /// projection declares no `$sort`. `None` when the `$view` is not a plain
    /// top-level projection whose ordering the check can read (a combinator or
    /// nested source is a documented seam, left uncompared to avoid over-rejection).
    ordering: Option<Vec<(String, bool)>>,
}

/// One exposed operation bound through a surface `$mut` (E.7): its accepted input
/// and the response members it promises. `response` is `None` when the mutation's
/// `return` is not a plain projection the check can read.
struct Operation {
    params: BTreeMap<String, Param>,
    response: Option<BTreeSet<String>>,
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
        let response = response_members(mutation);
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
            && a != c
        {
            return Some(format!(
                "surface `{address}` changes exposed row identity from {a:?} to {c:?}"
            ));
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
        for (member, active_ty) in &active_out.members {
            let Some(cand_ty) = cand_out.members.get(member) else {
                return Some(format!("surface `{address}` removes output member `{member}`"));
            };
            if output_narrows(active_ty, cand_ty) {
                return Some(format!("surface `{address}` narrows output member `{member}`"));
            }
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
        if let (Some(active_resp), Some(cand_resp)) = (&active_op.response, &cand_op.response) {
            for member in active_resp {
                if !cand_resp.contains(member) {
                    return Some(format!("{what} removes response member `{member}`"));
                }
            }
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

/// The response members a mutation's `return` projection promises (E.5/E.7), or
/// `None` when the trailing `return` is not a plain projection block.
fn response_members(mutation: &CompiledMutation) -> Option<BTreeSet<String>> {
    let ret = mutation.program.iter().rev().find_map(|stmt| match &stmt.stmt.kind {
        StmtKind::Return(expr) => Some(expr),
        _ => None,
    })?;
    projection_members(ret)
}

/// The output member names a projection block `base { ... }` declares, or `None`
/// when the expression is not a projection or names a member the check cannot
/// read (so an unreadable projection is treated as opaque, never a narrowing).
fn projection_members(expr: &Expr) -> Option<BTreeSet<String>> {
    let ExprKind::Block { members, .. } = &expr.kind else {
        return None;
    };
    let mut out = BTreeSet::new();
    for member in members {
        match &member.kind {
            BlockMemberKind::Named { name, .. } => {
                out.insert(name.text.clone());
            }
            BlockMemberKind::Shorthand(expr) => {
                out.insert(shorthand_member(expr)?);
            }
            // A directive/assign/clear does not name an output member.
            BlockMemberKind::Directive { .. } | BlockMemberKind::Assign { .. } | BlockMemberKind::Clear(_) => {}
        }
    }
    Some(out)
}

/// The output name a bare projection shorthand contributes (`id`, `row.field`).
fn shorthand_member(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Name(ident) | ExprKind::Param(ident) => Some(ident.text.clone()),
        ExprKind::Field { member, .. } => Some(member.text.clone()),
        _ => None,
    }
}

/// The exposed row identity of a surface view: the `$key` fields of the top-level
/// collection its projection reads (E.5). `None` when the source is not a plain
/// top-level collection.
fn exposed_identity(collection: &str, compiled: &Compiled) -> Option<Vec<String>> {
    compiled.collection(collection).map(|collection| collection.key.clone())
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

/// The exposed explicit ordering of a surface view (E.2/E.5): the ordered
/// `(key, descending)` sequence its top-level projection `$sort` declares, or an
/// empty sequence when the projection declares no `$sort`. `None` when the
/// `$view` is not a plain top-level projection whose ordering the check can read
/// (a combinator or nested source is a documented seam, left uncompared so a
/// compatible release is never mis-flagged). The `$sort` keys are normalized to
/// the key expression text with the direction extracted, so the string
/// (`"-name"`), compact (`-name`), and structured (`{ $by: name, $dir: desc }`)
/// §7.3 spellings of the same ordering compare equal.
fn view_ordering(decl: &DocValue) -> Option<Vec<(String, bool)>> {
    let text = doc::member(decl, "$view").and_then(doc::string)?;
    let mut sources = SourceMap::new();
    let source = sources.add_label("compat-ordering", text.to_owned());
    let parsed = parse_expression(source, text).ok()?;
    let StmtKind::Bare(expr) = &parsed.statement().kind else {
        return None;
    };
    let ExprKind::Block { members, .. } = &expr.kind else {
        return None;
    };
    for member in members {
        if let BlockMemberKind::Directive { name, value } = &member.kind
            && name.text == "sort"
        {
            return sort_keys(value, text);
        }
    }
    Some(Vec::new())
}

/// Normalize a projection `$sort` directive value — a §7.3 array of comparison
/// keys — into the ordered `(key, descending)` sequence. `None` when the value
/// is not the expected array form (a malformed `$sort` is left uncompared rather
/// than mis-flagged).
fn sort_keys(value: &Expr, text: &str) -> Option<Vec<(String, bool)>> {
    let ExprKind::List(items) = &value.kind else {
        return None;
    };
    items.iter().map(|item| sort_key(item, text)).collect()
}

/// Normalize one §7.3 sort key into `(key, descending)`. The three spellings all
/// reduce to the same pair: a string `"-field"`, a compact `-field`, and a
/// structured `{ $by: field, $dir: desc }` each yield `("field", true)`.
fn sort_key(item: &Expr, text: &str) -> Option<(String, bool)> {
    match &item.kind {
        // Canonical wire form: the string holds the key expression, a leading `-`
        // reversing it (§7.3).
        ExprKind::Str(spelling) => {
            let spelling = spelling.trim();
            match spelling.strip_prefix('-') {
                Some(body) => Some((body.trim().to_owned(), true)),
                None => Some((spelling.to_owned(), false)),
            }
        }
        // Compact DSL: a leading `-` reverses one key.
        ExprKind::Unary { op: UnaryOp::Neg, operand } => Some((key_text(operand, text)?, true)),
        // Structured form: `{ $by: field, $dir: asc|desc }`.
        ExprKind::Object(members) => structured_sort_key(members, text),
        // A bare ascending key expression.
        _ => Some((key_text(item, text)?, false)),
    }
}

/// Normalize a structured §7.3 sort key `{ $by: field, $dir: asc|desc }` into
/// `(key, descending)`. `None` when `$by` is absent.
fn structured_sort_key(members: &[BlockMember], text: &str) -> Option<(String, bool)> {
    let mut by = None;
    let mut descending = false;
    for member in members {
        let BlockMemberKind::Directive { name, value } = &member.kind else {
            continue;
        };
        match name.text.as_str() {
            "by" => {
                by = Some(match &value.kind {
                    ExprKind::Str(spelling) => spelling.trim().to_owned(),
                    _ => key_text(value, text)?,
                });
            }
            "dir" => {
                if let ExprKind::Str(spelling) = &value.kind {
                    descending = spelling.trim() == "desc";
                }
            }
            _ => {}
        }
    }
    Some((by?, descending))
}

/// The source text of a bare (non-string) sort-key expression, so a compact
/// `-field` or structured `$by: field` key compares equal to its string
/// spelling `"field"`. Outer whitespace is trimmed; inner spelling is compared
/// verbatim.
fn key_text(expr: &Expr, text: &str) -> Option<String> {
    let start = expr.span.start() as usize;
    let end = expr.span.end() as usize;
    text.get(start..end).map(|slice| slice.trim().to_owned())
}
