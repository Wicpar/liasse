//! The hoisting boundary (§7.3/§7.5 of `liasse-pg/DESIGN-pure-pg.md`).
//!
//! A lowered row-program's residual is evaluated once per candidate against a
//! minimal environment: the candidate row (`.` and the filter/coverage bind) and a
//! set of **hoisted** synthetic bindings — the values of every maximal
//! candidate-free subexpression that reaches outside the candidate (a `/`-read, a
//! session `@param`/`$actor`, a built-in namespace call). Because every built-in is
//! deterministic and `now()` is the fixed per-operation sample, one evaluation of a
//! candidate-free subtree equals `N`, so the compiler pre-evaluates it once and
//! ships the resulting [`Cell`](crate::Cell) as a synthetic binding.
//!
//! [`hoist`] performs the split: the residual [`TypedExpr`] with each such subtree
//! replaced by a synthetic `ScopeBinding`, plus the list of `(synthetic name,
//! candidate-free subtree)` the caller pre-evaluates. [`audit`] then confirms the
//! residual reaches nothing the minimal environment cannot serve — every remaining
//! *external* leaf must be candidate-dependent, and a candidate-dependent external
//! (an app host call over `.`, a placement member of a candidate blob) means the
//! source does not lower and the caller falls back to the interpreter (§7.5).

use std::collections::BTreeSet;

use crate::typed::{
    BlobMember, Output, Projection, SortKey, TypedExpr, TypedKind, TypedSelector, TypedTemporal,
};

/// Which references in a residual denote the candidate row (§7.3). For a flat
/// view's projection/sort and for a §10.5 coverage predicate the candidate is `.`
/// (and any bound name); for a flat view's FILTER `.` is the enclosing receiver
/// (candidate-free) and only the bind name denotes the candidate.
#[derive(Debug, Clone)]
pub struct CandidateRefs {
    /// Whether `.`/`^` (the current chain) denotes the candidate.
    pub current_is_candidate: bool,
    /// The binding names bound to the candidate (the filter/coverage bind, and any
    /// projected-output names threaded into a sort key).
    pub binds: BTreeSet<String>,
}

impl CandidateRefs {
    /// A classification where `.` is the candidate, plus the given bind names.
    #[must_use]
    pub fn current(binds: impl IntoIterator<Item = String>) -> Self {
        Self { current_is_candidate: true, binds: binds.into_iter().collect() }
    }

    /// A classification where `.` is the enclosing receiver (candidate-free), and
    /// only `binds` denote the candidate — a flat view's filter (§6.4).
    #[must_use]
    pub fn binds_only(binds: impl IntoIterator<Item = String>) -> Self {
        Self { current_is_candidate: false, binds: binds.into_iter().collect() }
    }
}

/// The result of hoisting: the residual and the candidate-free subtrees to
/// pre-evaluate, each under its synthetic binding name.
#[derive(Debug, Clone)]
pub struct Hoisted {
    /// The residual expression, evaluated per candidate.
    pub residual: TypedExpr,
    /// `(synthetic name, candidate-free subtree)` — the caller evaluates each once
    /// and binds the value under the synthetic name in the program env.
    pub entries: Vec<(String, TypedExpr)>,
}

/// Split `expr` into its candidate-dependent residual and the candidate-free
/// subexpressions to pre-evaluate, per `refs`. `next` is the shared synthetic-name
/// counter — one per lowered program, so a filter, its projection outputs, and its
/// sort keys never collide on a hoisted binding name.
#[must_use]
pub fn hoist(expr: &TypedExpr, refs: &CandidateRefs, next: &mut usize) -> Hoisted {
    let mut ctx = HoistCtx { refs, next, entries: Vec::new() };
    let residual = ctx.hoist_node(expr);
    Hoisted { residual, entries: ctx.entries }
}

/// Confirm `residual` reaches only what a minimal candidate + hoisted-env
/// evaluation can serve: no *external* leaf (a `/`-read, session binding, host
/// call, generative call, temporal/keyring/placement selector) may remain, since
/// any that survived hoisting is candidate-dependent and cannot be served. Returns
/// the offending kind's description on failure — the caller records it and falls
/// back to the interpreter (§7.5).
///
/// # Errors
///
/// Errors when the residual still reaches an external it cannot serve.
pub fn audit(residual: &TypedExpr) -> Result<(), &'static str> {
    match external_kind(residual) {
        Some(kind) => Err(kind),
        None => Ok(()),
    }
}

struct HoistCtx<'a> {
    refs: &'a CandidateRefs,
    next: &'a mut usize,
    entries: Vec<(String, TypedExpr)>,
}

impl HoistCtx<'_> {
    fn hoist_node(&mut self, expr: &TypedExpr) -> TypedExpr {
        // A maximal candidate-free subtree that reaches outside the candidate is
        // pre-evaluated once and replaced by a synthetic binding. A candidate-free
        // subtree that reaches nothing external (a pure constant expression) stays
        // in the residual — the minimal env evaluates it directly, per candidate,
        // exactly as the interpreter would.
        if !references_candidate(expr, self.refs) && external_kind(expr).is_some() {
            let name = format!("\0h{}", *self.next);
            *self.next += 1;
            self.entries.push((name.clone(), expr.clone()));
            return TypedExpr::new(expr.span(), expr.ty().clone(), TypedKind::ScopeBinding(name));
        }
        self.rebuild(expr)
    }

    /// Reconstruct `expr` with each direct child hoisted.
    fn rebuild(&mut self, expr: &TypedExpr) -> TypedExpr {
        let kind = match expr.kind() {
            TypedKind::Field { base, name } => {
                TypedKind::Field { base: self.child(base), name: name.clone() }
            }
            TypedKind::Select { base, selector } => {
                TypedKind::Select { base: self.child(base), selector: self.selector(selector) }
            }
            TypedKind::Traverse { base, member } => {
                TypedKind::Traverse { base: self.child(base), member: member.clone() }
            }
            TypedKind::Arith { op, class, lhs, rhs } => {
                TypedKind::Arith { op: *op, class: *class, lhs: self.child(lhs), rhs: self.child(rhs) }
            }
            TypedKind::Neg { class, operand } => {
                TypedKind::Neg { class: *class, operand: self.child(operand) }
            }
            TypedKind::Compare { op, lhs, rhs } => {
                TypedKind::Compare { op: *op, lhs: self.child(lhs), rhs: self.child(rhs) }
            }
            TypedKind::Logic { op, lhs, rhs } => {
                TypedKind::Logic { op: *op, lhs: self.child(lhs), rhs: self.child(rhs) }
            }
            TypedKind::Not(operand) => TypedKind::Not(self.child(operand)),
            TypedKind::In { needle, haystack } => {
                TypedKind::In { needle: self.child(needle), haystack: self.child(haystack) }
            }
            TypedKind::Ternary { cond, then, otherwise } => TypedKind::Ternary {
                cond: self.child(cond),
                then: self.child(then),
                otherwise: self.child(otherwise),
            },
            TypedKind::Aggregate { func, source, field } => TypedKind::Aggregate {
                func: *func,
                source: self.child(source),
                field: field.clone(),
            },
            TypedKind::Project { source, projection } => TypedKind::Project {
                source: self.child(source),
                projection: self.projection(projection),
            },
            TypedKind::Combine { op, lhs, rhs } => {
                TypedKind::Combine { op: *op, lhs: self.child(lhs), rhs: self.child(rhs) }
            }
            TypedKind::Fallback { primary, other } => {
                TypedKind::Fallback { primary: self.child(primary), other: self.child(other) }
            }
            TypedKind::List(items) => TypedKind::List(items.iter().map(|i| self.hoist_node(i)).collect()),
            TypedKind::Struct(fields) => TypedKind::Struct(
                fields.iter().map(|(name, e)| (name.clone(), self.hoist_node(e))).collect(),
            ),
            TypedKind::Composite { order, source } => {
                TypedKind::Composite { order: order.clone(), source: self.child(source) }
            }
            TypedKind::Builtin { func, args } => {
                TypedKind::Builtin { func: *func, args: args.iter().map(|a| self.hoist_node(a)).collect() }
            }
            TypedKind::HostCall { namespace, function, args } => TypedKind::HostCall {
                namespace: namespace.clone(),
                function: function.clone(),
                args: args.iter().map(|a| self.hoist_node(a)).collect(),
            },
            TypedKind::Temporal { base, query } => {
                TypedKind::Temporal { base: self.child(base), query: self.temporal(query) }
            }
            TypedKind::Key(base) => TypedKind::Key(self.child(base)),
            TypedKind::Keyring { base, selector } => {
                TypedKind::Keyring { base: self.child(base), selector: *selector }
            }
            TypedKind::BlobMember { base, member } => {
                TypedKind::BlobMember { base: self.child(base), member: *member }
            }
            // Leaves and nullary nodes reconstruct verbatim.
            other => other.clone(),
        };
        TypedExpr::new(expr.span(), expr.ty().clone(), kind)
    }

    fn child(&mut self, expr: &TypedExpr) -> Box<TypedExpr> {
        Box::new(self.hoist_node(expr))
    }

    fn selector(&mut self, selector: &TypedSelector) -> TypedSelector {
        match selector {
            TypedSelector::Keys(keys) => {
                TypedSelector::Keys(keys.iter().map(|k| self.hoist_node(k)).collect())
            }
            TypedSelector::Bind { name, condition } => TypedSelector::Bind {
                name: name.clone(),
                condition: condition.as_ref().map(|c| self.child(c)),
            },
        }
    }

    fn projection(&mut self, projection: &Projection) -> Projection {
        Projection {
            key: projection.key.clone(),
            outputs: projection
                .outputs
                .iter()
                .map(|o| Output { name: o.name.clone(), expr: self.hoist_node(&o.expr) })
                .collect(),
            quantity: projection.quantity.as_ref().map(|q| self.child(q)),
            sort: projection
                .sort
                .iter()
                .map(|k| SortKey { expr: self.hoist_node(&k.expr), descending: k.descending })
                .collect(),
            skip: projection.skip,
            limit: projection.limit,
        }
    }

    fn temporal(&mut self, query: &TypedTemporal) -> TypedTemporal {
        match query {
            TypedTemporal::At(t) => TypedTemporal::At(self.child(t)),
            TypedTemporal::Between { start, end } => {
                TypedTemporal::Between { start: self.child(start), end: self.child(end) }
            }
            TypedTemporal::All => TypedTemporal::All,
        }
    }
}

/// Whether `expr` anywhere references the candidate, per `refs`.
fn references_candidate(expr: &TypedExpr, refs: &CandidateRefs) -> bool {
    if is_candidate_leaf(expr.kind(), refs) {
        return true;
    }
    any_child(expr, &mut |child| references_candidate(child, refs))
}

fn is_candidate_leaf(kind: &TypedKind, refs: &CandidateRefs) -> bool {
    match kind {
        TypedKind::Current | TypedKind::Parent(_) => refs.current_is_candidate,
        TypedKind::ScopeBinding(name) | TypedKind::LocalBinding(name) => refs.binds.contains(name),
        _ => false,
    }
}

/// The description of the first *external* leaf reachable in `expr` — a node the
/// minimal candidate + hoisted-env evaluation cannot serve — or `None` when the
/// expression reaches only the candidate, hoisted synthetics, literals, and pure
/// operators/builtins over them.
fn external_kind(expr: &TypedExpr) -> Option<&'static str> {
    if let Some(kind) = own_external_kind(expr.kind()) {
        return Some(kind);
    }
    let mut found = None;
    any_child(expr, &mut |child| {
        if let Some(kind) = external_kind(child) {
            found = Some(kind);
            true
        } else {
            false
        }
    });
    found
}

/// The external classification of one node's own kind (ignoring children).
fn own_external_kind(kind: &TypedKind) -> Option<&'static str> {
    match kind {
        TypedKind::Root => Some("a `/` package-root read"),
        TypedKind::Param(_) => Some("a `@param` binding"),
        TypedKind::Structural(_) => Some("a `$actor`/`$session`/`$structural` binding"),
        TypedKind::Import(_) => Some("a `#import` binding"),
        TypedKind::HostCall { .. } => Some("a host-namespace call"),
        TypedKind::Now => Some("`now()`"),
        TypedKind::Uuid(_) => Some("`uuid()`"),
        TypedKind::Temporal { .. } => Some("a temporal selector"),
        TypedKind::Keyring { .. } => Some("a keyring selector"),
        // §18.5 placement members defer to the environment's placement index; the
        // §18.1 metadata members read off the descriptor directly and are not
        // external.
        TypedKind::BlobMember { member, .. } if is_placement_member(*member) => {
            Some("a blob placement member")
        }
        _ => None,
    }
}

fn is_placement_member(member: BlobMember) -> bool {
    matches!(member, BlobMember::Satisfied | BlobMember::Stored | BlobMember::Surplus)
}

/// Apply `f` to each direct child expression until one returns `true`; returns
/// whether any did. Used by the pure structural predicates above.
fn any_child(expr: &TypedExpr, f: &mut dyn FnMut(&TypedExpr) -> bool) -> bool {
    let mut hit = false;
    let mut visit = |child: &TypedExpr| {
        if !hit && f(child) {
            hit = true;
        }
    };
    match expr.kind() {
        TypedKind::Field { base, .. }
        | TypedKind::Traverse { base, .. }
        | TypedKind::Neg { operand: base, .. }
        | TypedKind::Not(base)
        | TypedKind::Composite { source: base, .. }
        | TypedKind::Key(base)
        | TypedKind::Temporal { base, .. }
        | TypedKind::Keyring { base, .. }
        | TypedKind::BlobMember { base, .. } => visit(base),
        TypedKind::Select { base, selector } => {
            visit(base);
            match selector {
                TypedSelector::Keys(keys) => keys.iter().for_each(&mut visit),
                TypedSelector::Bind { condition, .. } => {
                    if let Some(c) = condition {
                        visit(c);
                    }
                }
            }
        }
        TypedKind::Arith { lhs, rhs, .. }
        | TypedKind::Compare { lhs, rhs, .. }
        | TypedKind::Logic { lhs, rhs, .. }
        | TypedKind::Combine { lhs, rhs, .. } => {
            visit(lhs);
            visit(rhs);
        }
        TypedKind::In { needle, haystack } => {
            visit(needle);
            visit(haystack);
        }
        TypedKind::Ternary { cond, then, otherwise } => {
            visit(cond);
            visit(then);
            visit(otherwise);
        }
        TypedKind::Aggregate { source, .. } => visit(source),
        TypedKind::Fallback { primary, other } => {
            visit(primary);
            visit(other);
        }
        TypedKind::List(items) => items.iter().for_each(&mut visit),
        TypedKind::Struct(fields) => fields.iter().for_each(|(_, e)| visit(e)),
        TypedKind::Builtin { args, .. } | TypedKind::HostCall { args, .. } => {
            args.iter().for_each(&mut visit);
        }
        TypedKind::Project { source, projection } => {
            visit(source);
            projection.outputs.iter().for_each(|o| visit(&o.expr));
            projection.sort.iter().for_each(|k| visit(&k.expr));
            if let Some(q) = &projection.quantity {
                visit(q);
            }
        }
        TypedKind::Literal(_)
        | TypedKind::Root
        | TypedKind::Current
        | TypedKind::Parent(_)
        | TypedKind::Param(_)
        | TypedKind::Structural(_)
        | TypedKind::Import(_)
        | TypedKind::ScopeBinding(_)
        | TypedKind::LocalBinding(_)
        | TypedKind::EmptyView
        | TypedKind::Now
        | TypedKind::Uuid(_) => {}
    }
    hit
}
