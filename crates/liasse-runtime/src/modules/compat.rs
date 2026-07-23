//! §13.14 exposed-surface narrowing recheck and §13.15 `$exposed` grouping.
//!
//! A module's exposed compatibility surface (§13.8, Annex E.2) is its `$expose`
//! block: per interface, the fields its `$view` projects across the boundary and
//! the operations its `$mut` binds. §13.14 requires a same-major minor/patch
//! update to preserve or widen that surface; a narrowing release is refused before
//! the migration commits, so the instance keeps operating at its current version.
//!
//! This is a **purely definitional** comparison of the old and new exposed
//! surfaces read straight from the two definitions — independent of the child's
//! composition state — matching the split `tests/13-modules/NOTES.md` draws:
//!
//! - A narrowing the module makes to *its own* surface is a static
//!   ("package loading") refusal, mapped to `invalid`: an exposed `$view` that
//!   projects fewer fields, or an exposed operation the module no longer provides
//!   at all (its private backing mutation is gone from the candidate model too).
//!   The module has definitionally lost a promise ([`NarrowingClass::Definitional`]).
//! - An exposed operation whose binding is *withdrawn* while the module still
//!   carries a private mutation that could satisfy it is "well-formed on its own":
//!   the defect is only the removed boundary binding (E.4 "removing a previously
//!   accepted module-interface binding"), caught by the §13.14 recheck of module
//!   exposures before admission and mapped to `rejected`
//!   ([`NarrowingClass::BindingWithdrawn`]).
//!
//! §13.15's `$exposed` report groups every exposed interface by how its contract
//! moved across the accepted update — `$unchanged`, `$changed` (widened), or
//! `$removed` — read from the same two surfaces ([`exposed_grouping`]).

use std::collections::{BTreeMap, BTreeSet};

use liasse_diag::SourceMap;
use liasse_syntax::{parse_document, parse_expression, BlockMemberKind, Expr, ExprKind, StmtKind};

use crate::doc;

/// One module's exposed compatibility surface (§13.8/E.2): per interface, the
/// projected `$view` output field names and the exposed `$mut` operation bindings.
struct ExposedSurface {
    interfaces: BTreeMap<String, ExposedInterface>,
}

/// One exposed interface's boundary contract (§13.8): the field names its `$view`
/// projects across the boundary, and each exposed operation mapped to the private
/// binding text it names (`create` -> `.create_template`).
struct ExposedInterface {
    view_fields: BTreeSet<String>,
    operations: BTreeMap<String, String>,
}

/// How an exposed-surface narrowing is classified for the update outcome
/// (`tests/13-modules/NOTES.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NarrowingClass {
    /// The module narrowed its own surface definitionally — a static refusal
    /// (`invalid`): a dropped view field, or an operation whose backing private
    /// mutation is also gone.
    Definitional,
    /// A previously accepted interface binding was withdrawn while the private
    /// implementation remains — an admission-recheck refusal (`rejected`).
    BindingWithdrawn,
}

/// One detected exposed-surface narrowing (§13.14): a human-readable reason and
/// its outcome classification.
pub(crate) struct Narrowing {
    pub(crate) reason: String,
    pub(crate) class: NarrowingClass,
}

/// The `$exposed` grouping of a successful update (§13.15): every exposed
/// interface bucketed by how its contract moved, each bucket in canonical text
/// order.
pub(crate) struct ExposedGrouping {
    pub(crate) unchanged: Vec<String>,
    pub(crate) changed: Vec<String>,
    pub(crate) removed: Vec<String>,
}

/// The first exposed-surface narrowing the `candidate` definition makes relative
/// to the `active` one (§13.14), or `None` when it preserves or widens every
/// exposed interface. A definition whose `$expose`/`$model` cannot be read
/// contributes no surface, so an unreadable pair simply reports no narrowing (the
/// migration then runs its ordinary §20 pipeline).
pub(crate) fn exposed_narrowing(active: &str, candidate: &str) -> Option<Narrowing> {
    let active_surface = ExposedSurface::read(active);
    let candidate_surface = ExposedSurface::read(candidate);
    let candidate_muts = model_mutations(candidate);
    for (name, active_iface) in &active_surface.interfaces {
        let Some(candidate_iface) = candidate_surface.interfaces.get(name) else {
            // The whole interface is no longer exposed — the module definitionally
            // withdrew every promise it carried (E.5 "removing a view or mutation
            // bound by a module interface").
            return Some(Narrowing {
                reason: format!("exposed interface `{name}` is removed"),
                class: NarrowingClass::Definitional,
            });
        };
        // E.5: an exposed view that projects fewer fields narrows the observable
        // output shape — a definitional self-narrowing regardless of what a parent
        // requires.
        if let Some(field) = active_iface.view_fields.difference(&candidate_iface.view_fields).next() {
            return Some(Narrowing {
                reason: format!("exposed interface `{name}` view drops field `{field}`"),
                class: NarrowingClass::Definitional,
            });
        }
        // E.4/E.7: an exposed operation the candidate no longer binds. Whether this
        // is a definitional loss (`invalid`) or a withdrawn-binding admission
        // refusal (`rejected`) turns on whether the module still carries a private
        // mutation that could satisfy it — the split `tests/13-modules/NOTES.md`
        // draws between the two narrowing outcomes.
        for (operation, binding) in &active_iface.operations {
            if candidate_iface.operations.contains_key(operation) {
                continue;
            }
            let class = if backing_mutation(binding).is_some_and(|name| candidate_muts.contains(name)) {
                NarrowingClass::BindingWithdrawn
            } else {
                NarrowingClass::Definitional
            };
            return Some(Narrowing {
                reason: format!("exposed interface `{name}` no longer binds operation `{operation}`"),
                class,
            });
        }
    }
    None
}

/// Group every interface the `active` definition exposed by how its contract moved
/// to the `candidate` (§13.15 `$exposed`). An accepted update never narrows (that
/// is refused before commit), so each name is `$unchanged` (identical contract) or
/// `$changed` (a compatible widening — an added field or operation); an interface
/// the candidate no longer exposes is `$removed`. Each bucket is in canonical text
/// order.
pub(crate) fn exposed_grouping(active: &str, candidate: &str) -> ExposedGrouping {
    let active_surface = ExposedSurface::read(active);
    let candidate_surface = ExposedSurface::read(candidate);
    let mut grouping = ExposedGrouping { unchanged: Vec::new(), changed: Vec::new(), removed: Vec::new() };
    for (name, active_iface) in &active_surface.interfaces {
        match candidate_surface.interfaces.get(name) {
            None => grouping.removed.push(name.clone()),
            Some(candidate_iface) if active_iface.same_contract(candidate_iface) => {
                grouping.unchanged.push(name.clone());
            }
            Some(_) => grouping.changed.push(name.clone()),
        }
    }
    grouping
}

impl ExposedInterface {
    /// Whether this interface's exposed contract is identical to `other`'s: the
    /// same projected view fields and the same set of exposed operation names
    /// (§13.15 `$unchanged`). A rebind to a different private implementation under
    /// the same operation name keeps the contract unchanged (E.4/E.7 — "rebinding
    /// an interface mutation to a different private implementation that satisfies
    /// the same contract"), so only the operation names are compared, not their
    /// binding text.
    fn same_contract(&self, other: &Self) -> bool {
        self.view_fields == other.view_fields && self.operations.keys().eq(other.operations.keys())
    }

    /// Read one exposed interface's `$view` output fields and `$mut` operation
    /// bindings from its `$expose` object.
    fn read(decl: &liasse_syntax::DocValue) -> Self {
        let view_fields = doc::member(decl, "$view")
            .and_then(doc::string)
            .map(view_output_fields)
            .unwrap_or_default();
        let mut operations = BTreeMap::new();
        if let Some(muts) = doc::member(decl, "$mut").and_then(doc::object) {
            for entry in muts {
                // An inline mutation-expression binding (§13.8) carries no simple ref
                // name; record it with an empty binding so its presence still counts
                // as an exposed operation the candidate must preserve.
                let binding = doc::string(&entry.value).unwrap_or_default().to_owned();
                operations.insert(entry.name.text.clone(), binding);
            }
        }
        Self { view_fields, operations }
    }
}

impl ExposedSurface {
    /// Read a definition's top-level `$expose` block into its exposed surface. A
    /// definition with no readable `$expose` yields an empty surface (no exposed
    /// promises), so its comparison reports neither a narrowing nor a grouped
    /// interface.
    fn read(definition: &str) -> Self {
        let mut interfaces = BTreeMap::new();
        if let Some(document) = parse_definition(definition)
            && let Some(expose) = doc::member(&document, "$expose")
            && let Some(members) = doc::object(expose)
        {
            for member in members {
                interfaces.insert(member.name.text.clone(), ExposedInterface::read(&member.value));
            }
        }
        Self { interfaces }
    }
}

/// Parse a definition's top-level document, or `None` when it does not parse.
fn parse_definition(definition: &str) -> Option<liasse_syntax::DocValue> {
    let mut sources = SourceMap::new();
    let src = sources.add_file("liasse.json", definition.to_owned());
    parse_document(src, definition).ok().map(|document| document.root().clone())
}

/// The mutation names a definition declares in its `$model.$mut` block — the
/// private implementations an exposed operation may bind (§13.8).
fn model_mutations(definition: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(document) = parse_definition(definition)
        && let Some(model) = doc::member(&document, "$model")
        && let Some(muts) = doc::member(model, "$mut").and_then(doc::object)
    {
        for entry in muts {
            names.insert(entry.name.text.clone());
        }
    }
    names
}

/// The private mutation name an exposed `$mut` binding references (`.create_template`
/// -> `create_template`, `.templates[@t].disable()` -> `disable`). `None` when the
/// binding is not a simple dotted mutation reference (an inline expression), whose
/// backing implementation cannot be named definitionally.
fn backing_mutation(binding: &str) -> Option<&str> {
    let text = binding.trim();
    let text = text.strip_prefix('=').map_or(text, str::trim);
    // A simple binding is a `.`-rooted reference; an inline mutation expression is
    // not, so it has no nameable backing mutation.
    if !text.starts_with('.') {
        return None;
    }
    let text = text.strip_suffix("()").unwrap_or(text);
    let tail = text.rsplit('.').next()?.trim();
    if tail.is_empty() || !tail.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some(tail)
}

/// The output field names an exposed `$view` projection declares (§13.8): the
/// members of its trailing `{ ... }` projection, each named or bare-shorthand
/// member contributing its output name; a `$`-directive (`$sort`, `$key`, …) is
/// not an output field. A `$view` that is not a plain projection contributes no
/// readable fields.
fn view_output_fields(text: &str) -> BTreeSet<String> {
    let mut sources = SourceMap::new();
    let source = sources.add_label("expose-view", text.to_owned());
    let Ok(parsed) = parse_expression(source, text) else {
        return BTreeSet::new();
    };
    let StmtKind::Bare(expr) = &parsed.statement().kind else {
        return BTreeSet::new();
    };
    let ExprKind::Block { members, .. } = &expr.kind else {
        return BTreeSet::new();
    };
    members
        .iter()
        .filter_map(|member| match &member.kind {
            BlockMemberKind::Named { name, .. } => Some(name.text.clone()),
            BlockMemberKind::Shorthand(expr) => shorthand_output_name(expr),
            BlockMemberKind::Directive { .. } | BlockMemberKind::Clear(_) | BlockMemberKind::Assign { .. } => None,
        })
        .collect()
}

/// The output field name a bare projection shorthand contributes: the trailing
/// member of a field access (`binding.field` -> `field`, `.field` -> `field`) or a
/// bare name. A more complex shorthand expression contributes no simple name.
fn shorthand_output_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Field { member, .. } => Some(member.text.clone()),
        ExprKind::Name(ident) => Some(ident.text.clone()),
        _ => None,
    }
}
