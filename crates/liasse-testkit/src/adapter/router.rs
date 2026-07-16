//! Deriving a public [`SurfaceRouter`] from a loaded package.
//!
//! The surface layer wires a router by hand (a [`CallBinding`] per exposed call,
//! naming the runtime mutation and its receiver/parameter split). This module
//! reconstructs that wiring from the model plus the raw `$public` block, so the
//! adapter routes the same path a production host would: a `$mut` reference names
//! a declared mutation, whose parameter contract the model already validated, and
//! a bare `$view` reference names a declared view.
//!
//! Scope for this phase: public surfaces only. Authenticators, roles, and inline
//! (unnamed) surface views need host wiring (`$verify` namespaces, session
//! sources) that Engine load does not yet consume, so a role-scoped or
//! inline-view surface is left unbound — its calls resolve to a `denied` outcome,
//! which is a faithful observation rather than a fabricated one.

use std::collections::{BTreeMap, BTreeSet};

use liasse_diag::SourceMap;
use liasse_model::{Model, Node};
use liasse_surface::{
    CallBinding, RouterError, SurfaceBinding, SurfaceRouter, SurfaceRouterBuilder, ViewBinding,
};
use liasse_syntax::{parse_expression, BlockMemberKind, Expr, ExprKind, Selector, StmtKind};
use liasse_value::Type;
use serde_json::Value as J;

use super::auth::AuthPlan;
use super::lift::SurfaceLift;

/// The argument-type tables the adapter decodes `args` against, keyed by the full
/// dotted call address (`public.<surface>.<call>`).
#[derive(Debug, Clone, Default)]
pub struct Routing {
    call_arg_types: BTreeMap<String, BTreeMap<String, Type>>,
}

impl Routing {
    /// The declared argument types for the call at `address`, or an empty table
    /// when the address is unknown (arguments are then shape-inferred).
    #[must_use]
    pub fn arg_types(&self, address: &str) -> BTreeMap<String, Type> {
        self.call_arg_types.get(address).cloned().unwrap_or_default()
    }
}

/// The per-call argument-type tables one surface contributes, each keyed by the
/// full dotted call address.
type SurfaceArgTypes = Vec<(String, BTreeMap<String, Type>)>;

/// One declared mutation's shape, indexed by name for reference resolution.
struct MutInfo {
    name: String,
    params: Vec<(String, Option<Type>)>,
    /// The receiver collection's `$key` field types, in `$key` order (empty for
    /// a root or struct mutation). A selector parameter selecting the receiver
    /// row decodes as the same-position key field's type.
    receiver_types: Vec<Type>,
}

/// Build the public router for `model`/`package` and the argument-type tables the
/// adapter decodes against. `plan` carries the host-free authentication wiring
/// (§11): its authenticators and roles are registered, and each wired role's
/// surfaces are bound the same way public ones are.
pub fn build(
    model: &Model,
    package: &J,
    plan: &AuthPlan,
    lift: &SurfaceLift,
) -> Result<(SurfaceRouter, Routing), RouterError> {
    let muts = mutation_index(model);
    let views = declared_views(model);
    let state = package.get("$model");
    let public = state.and_then(|m| m.get("$public")).and_then(J::as_object);
    let roles = state.and_then(|m| m.get("$roles")).and_then(J::as_object);

    let mut builder = SurfaceRouterBuilder::new();
    let mut routing = Routing::default();

    if let Some(public) = public {
        for (surface, definition) in public {
            let (binding, arg_types) =
                surface_binding("public", surface, definition, &muts, &views, lift);
            for (address, types) in arg_types {
                routing.call_arg_types.insert(address, types);
            }
            builder = builder.public_surface(surface.clone(), binding);
        }
    }

    for authenticator in plan.authenticators() {
        builder = builder.authenticator(authenticator);
    }
    for role in plan.roles() {
        let name = role.name().to_owned();
        let surfaces = roles
            .and_then(|roles| roles.get(&name))
            .map(|definition| role_surfaces(&name, definition, &muts, &views, lift, &mut routing))
            .unwrap_or_default();
        builder = builder.role(role, surfaces);
    }

    Ok((builder.build(model)?, routing))
}

/// Assemble the surface bindings a role grants, recording their per-call argument
/// types under the role's dotted address (`<role>.<surface>.<call>`). A role
/// member is a surface unless it is a `$`-prefixed control key.
fn role_surfaces(
    role: &str,
    definition: &J,
    muts: &BTreeMap<String, MutInfo>,
    views: &BTreeSet<String>,
    lift: &SurfaceLift,
    routing: &mut Routing,
) -> Vec<(String, SurfaceBinding)> {
    let Some(members) = definition.as_object() else { return Vec::new() };
    let mut surfaces = Vec::new();
    for (name, surface) in members {
        if name.starts_with('$') {
            continue;
        }
        let (binding, arg_types) = surface_binding(role, name, surface, muts, views, lift);
        for (address, types) in arg_types {
            routing.call_arg_types.insert(address, types);
        }
        surfaces.push((name.clone(), binding));
    }
    surfaces
}

/// Assemble one surface's binding and the per-call argument-type tables, keyed
/// under `address_prefix` (`public`, or a role name).
fn surface_binding(
    address_prefix: &str,
    surface: &str,
    definition: &J,
    muts: &BTreeMap<String, MutInfo>,
    views: &BTreeSet<String>,
    lift: &SurfaceLift,
) -> (SurfaceBinding, SurfaceArgTypes) {
    let mut binding = SurfaceBinding::new();
    let mut arg_types = Vec::new();
    let surface_address = format!("{address_prefix}.{surface}");

    // An inline `$view` was lifted to a synthetic top-level view; a bare
    // reference to an already-declared view binds directly.
    if let Some(name) = lift.view_name(&surface_address) {
        binding = binding.with_view(ViewBinding::new(name));
    } else if let Some(view) = definition.get("$view").and_then(J::as_str)
        && let Some(name) = bare_reference(view)
        && views.contains(name)
    {
        binding = binding.with_view(ViewBinding::new(name));
    }

    if let Some(calls) = definition.get("$mut").and_then(J::as_object) {
        for call in calls.keys() {
            let call_address = format!("{address_prefix}.{surface}.{call}");
            let Some((call_binding, types)) = call_binding(&calls[call], &call_address, muts, lift)
            else {
                continue;
            };
            binding = binding.with_call(call.clone(), call_binding);
            arg_types.push((call_address, types));
        }
    }

    (binding, arg_types)
}

/// The [`CallBinding`] and argument types for one surface `$mut` member. An
/// inline program lifted to a synthetic root mutation binds every parameter as
/// an argument; a declared-mutation reference binds the receiver row from the
/// arguments its selector names (in `$key` order), combined with the mutation's
/// own parameters (§10.1).
fn call_binding(
    body: &J,
    call_address: &str,
    muts: &BTreeMap<String, MutInfo>,
    lift: &SurfaceLift,
) -> Option<(CallBinding, BTreeMap<String, Type>)> {
    if let Some(info) = lift.mut_name(call_address).and_then(|name| muts.get(name)) {
        return Some(root_binding(info));
    }
    let reference = parse_reference(body.as_str()?)?;
    let info = muts.get(&reference.mutation)?;
    Some(row_binding(info, &reference.receiver))
}

/// A root-mutation binding: no receiver, every parameter passed as an argument.
fn root_binding(info: &MutInfo) -> (CallBinding, BTreeMap<String, Type>) {
    let params: Vec<String> = info.params.iter().map(|(param, _)| param.clone()).collect();
    let types = param_types(info);
    (CallBinding::root(info.name.clone(), params), types)
}

/// A binding for a declared-mutation reference: `receiver` names the arguments
/// selecting the receiver row (empty for a root mutation), and the mutation's
/// own parameters follow. Each receiver argument decodes as its same-position
/// `$key` field type (§6.3), so a `uuid`/`int` key is matched, not stringified.
fn row_binding(info: &MutInfo, receiver: &[String]) -> (CallBinding, BTreeMap<String, Type>) {
    let params: Vec<String> = info.params.iter().map(|(param, _)| param.clone()).collect();
    let mut types = param_types(info);
    if receiver.is_empty() {
        return (CallBinding::root(info.name.clone(), params), types);
    }
    for (position, key) in receiver.iter().enumerate() {
        let ty = info.receiver_types.get(position).cloned().unwrap_or(Type::Text);
        types.entry(key.clone()).or_insert(ty);
    }
    (CallBinding::row(info.name.clone(), receiver.to_vec(), params), types)
}

/// The declared argument types of a mutation's parameters (those the model
/// resolved to a scalar type).
fn param_types(info: &MutInfo) -> BTreeMap<String, Type> {
    info.params
        .iter()
        .filter_map(|(param, ty)| ty.clone().map(|ty| (param.clone(), ty)))
        .collect()
}

/// Index every declared mutation by its external name.
fn mutation_index(model: &Model) -> BTreeMap<String, MutInfo> {
    let mut index = BTreeMap::new();
    for mutation in model.mutations() {
        let name = mutation.name.as_str().to_owned();
        let params = mutation
            .params
            .iter()
            .map(|(param, ty)| (param.clone(), ty.as_scalar().cloned()))
            .collect();
        let receiver_types = collection_key_types(model, &mutation.path);
        index.insert(name.clone(), MutInfo { name, params, receiver_types });
    }
    index
}

/// The `$key` field types of the collection at `path` (the mutation's receiver
/// location), in `$key` order. A root/struct mutation (empty or non-collection
/// path) has no receiver key.
fn collection_key_types(model: &Model, path: &[String]) -> Vec<Type> {
    let mut shape = model.root();
    let mut collection = None;
    for segment in path {
        let Some(member) = shape.member(segment) else { return Vec::new() };
        match &member.node {
            Node::Collection(next) => {
                shape = &next.shape;
                collection = Some(next.as_ref());
            }
            Node::Struct(next) => {
                shape = next;
                collection = None;
            }
            _ => return Vec::new(),
        }
    }
    let Some(collection) = collection else { return Vec::new() };
    collection
        .key
        .iter()
        .map(|field| match shape.member(field.as_str()).map(|member| &member.node) {
            Some(Node::Scalar(scalar)) => scalar.ty.clone(),
            _ => Type::Text,
        })
        .collect()
}

/// The set of top-level declared view names.
fn declared_views(model: &Model) -> BTreeSet<String> {
    model
        .root()
        .members
        .iter()
        .filter(|member| matches!(member.node, Node::View(_)))
        .map(|member| member.name.as_str().to_owned())
        .collect()
}

/// A declared-mutation surface reference resolved to its bindable shape (§10.1):
/// the mutation name, and the argument names selecting the receiver row in
/// `$key` order (empty for a root or struct mutation).
struct ReferenceBinding {
    mutation: String,
    receiver: Vec<String>,
}

/// Parse a surface `$mut` reference string into its bindable shape. Returns
/// `None` when the reference is not a plain receiver-and-parameters call the
/// [`CallBinding`] can express — an explicit call carrying fixed/derived
/// arguments (`.c[@k].m({ f: … })`) or a filter selector (`.c[:x | …].m`) needs
/// richer wiring the surface layer does not model, so it is left unbound.
fn parse_reference(text: &str) -> Option<ReferenceBinding> {
    let mut sources = SourceMap::new();
    let source = sources.add_label("surface-ref", text.to_owned());
    let parsed = parse_expression(source, text).ok()?;
    let StmtKind::Bare(expr) = &parsed.statement().kind else { return None };
    // A bare `.base.mutation` reference, or an explicit call `.base.mutation()`
    // with no arguments (equivalent to the bare reference). An explicit call
    // carrying arguments renames/derives/fixes them (§10.1), which the
    // CallBinding cannot model, so it is left unbound.
    let reference = match &expr.kind {
        ExprKind::Field { .. } => expr,
        ExprKind::Call { callee, args } if args.is_empty() => callee.as_ref(),
        _ => return None,
    };
    let ExprKind::Field { base, member } = &reference.kind else { return None };
    let receiver = receiver_args(base)?;
    Some(ReferenceBinding { mutation: member.text.clone(), receiver })
}

/// The argument names a reference base selects as the receiver row, in `$key`
/// order: none for a root/struct receiver (`.` / `/` / a bare name), or the key
/// selector's parameters. A filter selector or a non-parameter key yields `None`.
fn receiver_args(base: &Expr) -> Option<Vec<String>> {
    match &base.kind {
        ExprKind::Current | ExprKind::Root | ExprKind::Name(_) => Some(Vec::new()),
        ExprKind::Field { base, .. } => receiver_args(base),
        ExprKind::Select { selector: Selector::Keys(keys), .. } => key_params(keys),
        _ => None,
    }
}

/// The parameter names of a key selector, in order: `[@a, @b]` names two
/// parameters; a composite-key object `[{ f: @a, g: @b }]` names its members'
/// parameters. A literal or computed key (not a bare `@param`) yields `None`.
fn key_params(keys: &[Expr]) -> Option<Vec<String>> {
    let mut params = Vec::new();
    for key in keys {
        match &key.kind {
            ExprKind::Param(id) => params.push(id.text.clone()),
            ExprKind::Object(members) => {
                for member in members {
                    let BlockMemberKind::Named { value: Some(value), .. } = &member.kind else {
                        return None;
                    };
                    let ExprKind::Param(id) = &value.kind else { return None };
                    params.push(id.text.clone());
                }
            }
            _ => return None,
        }
    }
    Some(params)
}

/// The identifier the reference points at when it is a bare `.<name>` with no
/// selector, call, or projection — the form a `$view` binding accepts.
fn bare_reference(text: &str) -> Option<&str> {
    let name = text.strip_prefix('.')?;
    (!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')).then_some(name)
}

/// The maximal `[A-Za-z0-9_]` runs of `text`, left to right — the reference's
/// identifier tokens with selectors, operators, and punctuation dropped.
pub(super) fn identifier_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (index, byte) in text.bytes().enumerate() {
        let is_ident = byte.is_ascii_alphanumeric() || byte == b'_';
        match (is_ident, start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                if let Some(token) = text.get(begin..index) {
                    tokens.push(token);
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start
        && let Some(token) = text.get(begin..)
    {
        tokens.push(token);
    }
    tokens
}
