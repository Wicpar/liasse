//! Compilation and prospective-state resolution of `$blob_storage` (§18.2–§18.4).
//!
//! The model layer validates these declarations but intentionally keeps no raw
//! document nodes. This module is the once-per-definition semantic boundary that
//! turns them into accepted types and typed store-view expressions retained by
//! the runtime.

use std::collections::BTreeMap;

use liasse_diag::SourceMap;
use liasse_expr::{Cell, ExprType, TypedExpr};
use liasse_syntax::{
    BlockMember, BlockMemberKind, DocValue, Expr, ExprKind, Selector, Stmt, StmtKind,
};
use liasse_value::Value;

use super::{CompiledMutation, CompiledStmt};
use crate::blobs::{AcceptedType, Placement, PlacementPolicy, Store, StoreId};
use crate::error::{EngineError, Rejection, RejectionReason};
use crate::eval::EvalCtx;
use crate::schema::Schema;
use crate::state::Prospective;

mod compiler;

/// All package blob fields that have both an accepted type and inherited
/// placement policy.
#[derive(Default)]
pub(crate) struct CompiledBlobs {
    fields: Vec<CompiledBlobField>,
}

impl CompiledBlobs {
    pub(crate) fn compile(
        sources: &mut SourceMap,
        schema: Schema<'_>,
        root: &ExprType,
        model: &DocValue,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            fields: compiler::compile(sources, schema, root, model)?,
        })
    }

    /// Resolve a mutation blob parameter to the semantic field it feeds. The
    /// parameter is routed by the field name(s) the mutation program assigns it to
    /// (§18.7): a package's blob parameter need not share its destination field's
    /// spelling (`.docs + { file: @payload }`, finding 3), so `@payload` resolves
    /// the `file` field, while a parameter spelled like its field (or an `@p`
    /// shorthand) resolves the same field. A field on the mutation receiver wins;
    /// otherwise a package-wide unique field is accepted. Ambiguity — several
    /// destination fields, or several like-named fields — is refused rather than
    /// choosing an arbitrary placement.
    pub(crate) fn field(
        &self,
        mutation: &CompiledMutation,
        parameter: &str,
    ) -> Result<&CompiledBlobField, Rejection> {
        // The field name(s) the program routes the parameter into. A parameter no
        // assignment routes (e.g. one read only as metadata) falls back to the
        // field spelled like it, preserving name-matched resolution.
        let mut names = dest_field_names(&mutation.program, parameter);
        if names.is_empty() {
            names.push(parameter.to_owned());
        }
        let matches =
            |field: &CompiledBlobField| names.iter().any(|name| name.as_str() == field.name());
        let receiver: Vec<&CompiledBlobField> = self
            .fields
            .iter()
            .filter(|field| matches(field) && field.parent_path() == mutation.path.as_slice())
            .collect();
        match receiver.as_slice() {
            [field] => return Ok(*field),
            [] => {}
            _ => return Err(ambiguous(parameter)),
        }
        let named: Vec<&CompiledBlobField> = self.fields.iter().filter(|f| matches(f)).collect();
        match named.as_slice() {
            [field] => Ok(*field),
            [] => Err(Rejection::new(
                RejectionReason::Malformed,
                format!(
                    "blob parameter `@{parameter}` has no accepted field under a `$blob_storage` declaration"
                ),
            )),
            _ => Err(ambiguous(parameter)),
        }
    }

    pub(crate) fn has_field(&self, name: &str) -> bool {
        self.fields.iter().any(|field| field.name() == name)
    }

    /// Every store row reachable from every declared placement in `prospective`.
    /// This is the §18.3 eager connector-validation set.
    pub(crate) fn reachable_stores(
        &self,
        ctx: &EvalCtx<'_>,
        prospective: &Prospective,
    ) -> Result<Vec<Store>, Rejection> {
        let mut stores = BTreeMap::new();
        for field in &self.fields {
            field.policy.resolve(ctx, prospective, &mut stores)?;
        }
        Ok(stores.into_values().collect())
    }
}

fn ambiguous(parameter: &str) -> Rejection {
    Rejection::new(
        RejectionReason::Malformed,
        format!(
            "blob parameter `@{parameter}` matches several accepted fields; declare it on the mutation receiver"
        ),
    )
}

/// The names of the fields a mutation program routes the blob parameter
/// `@parameter` into (§18.7): a `{ field: @p }` / `field = @p` insert-or-patch
/// member, a `.field = @p` statement, or the `@p` shorthand (which assigns the
/// like-named field, §8.6). A blob parameter feeds one accepted field, so these
/// destination names let an aliased parameter resolve to its field by binding
/// rather than by spelling (finding 3).
fn dest_field_names(program: &[CompiledStmt], parameter: &str) -> Vec<String> {
    let mut names = Vec::new();
    for statement in program {
        stmt_dests(&statement.stmt, parameter, &mut names);
    }
    names.sort();
    names.dedup();
    names
}

fn stmt_dests(stmt: &Stmt, parameter: &str, names: &mut Vec<String>) {
    match &stmt.kind {
        StmtKind::Assign { target, value } => {
            // `.field = @p` patches `field` on the receiver (§8.6).
            if is_param(value, parameter)
                && let ExprKind::Field { member, .. } = &target.kind
                && !member.structural
            {
                names.push(member.text.clone());
            }
            expr_dests(target, parameter, names);
            expr_dests(value, parameter, names);
        }
        // A move `dest <- source` lands its source exactly where an assignment's
        // value would (§8.5): a moved blob parameter reaches the same field.
        StmtKind::Move { dest, source } => {
            if is_param(source, parameter)
                && let ExprKind::Field { member, .. } = &dest.kind
                && !member.structural
            {
                names.push(member.text.clone());
            }
            expr_dests(dest, parameter, names);
            expr_dests(source, parameter, names);
        }
        StmtKind::Return(expr) | StmtKind::Clear(expr) | StmtKind::Bare(expr) => {
            expr_dests(expr, parameter, names);
        }
    }
}

fn expr_dests(expr: &Expr, parameter: &str, names: &mut Vec<String>) {
    match &expr.kind {
        ExprKind::Object(members) => {
            members
                .iter()
                .for_each(|member| member_dests(member, parameter, names));
        }
        ExprKind::Block { base, members } => {
            expr_dests(base, parameter, names);
            members
                .iter()
                .for_each(|member| member_dests(member, parameter, names));
        }
        ExprKind::List(items)
        | ExprKind::Combination {
            operands: items, ..
        } => {
            items
                .iter()
                .for_each(|item| expr_dests(item, parameter, names));
        }
        ExprKind::Field { base, .. } | ExprKind::SameName { base, .. } => {
            expr_dests(base, parameter, names);
        }
        ExprKind::Select { base, selector } => {
            expr_dests(base, parameter, names);
            match selector {
                Selector::Keys(keys) => keys
                    .iter()
                    .for_each(|key| expr_dests(key, parameter, names)),
                Selector::Bind {
                    condition: Some(condition),
                    ..
                } => {
                    expr_dests(condition, parameter, names);
                }
                Selector::Bind {
                    condition: None, ..
                } => {}
            }
        }
        ExprKind::Call { callee, args } => {
            expr_dests(callee, parameter, names);
            args.iter()
                .for_each(|arg| expr_dests(super::arg_value(arg), parameter, names));
        }
        ExprKind::Unary { operand, .. } => expr_dests(operand, parameter, names),
        ExprKind::Binary { lhs, rhs, .. } => {
            expr_dests(lhs, parameter, names);
            expr_dests(rhs, parameter, names);
        }
        ExprKind::Ternary {
            cond,
            then,
            otherwise,
        } => {
            expr_dests(cond, parameter, names);
            expr_dests(then, parameter, names);
            expr_dests(otherwise, parameter, names);
        }
        _ => {}
    }
}

fn member_dests(member: &BlockMember, parameter: &str, names: &mut Vec<String>) {
    match &member.kind {
        // `field: @p` — `@p` feeds the named field.
        BlockMemberKind::Named {
            name,
            value: Some(value),
        } => {
            if is_param(value, parameter) {
                names.push(name.text.clone());
            }
            expr_dests(value, parameter, names);
        }
        // `field = @p` — a patch assignment feeds the named field.
        BlockMemberKind::Assign { target, value } => {
            if is_param(value, parameter) {
                names.push(target.text.clone());
            }
            expr_dests(value, parameter, names);
        }
        // `@p` shorthand assigns the like-named field (`p = @p`, §8.6).
        BlockMemberKind::Shorthand(value) => {
            if is_param(value, parameter) {
                names.push(parameter.to_owned());
            }
            expr_dests(value, parameter, names);
        }
        BlockMemberKind::Directive { value, .. } => expr_dests(value, parameter, names),
        BlockMemberKind::Named { value: None, .. } | BlockMemberKind::Clear(_) => {}
    }
}

fn is_param(expr: &Expr, parameter: &str) -> bool {
    matches!(&expr.kind, ExprKind::Param(id) if id.text == parameter)
}

/// One accepted blob field and the nearest inherited placement declaration.
pub(crate) struct CompiledBlobField {
    path: Vec<String>,
    accepted: AcceptedType,
    policy: CompiledPolicy,
}

impl CompiledBlobField {
    pub(crate) fn name(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }

    fn parent_path(&self) -> &[String] {
        self.path
            .get(..self.path.len().saturating_sub(1))
            .unwrap_or_default()
    }

    pub(crate) fn accepted(&self) -> &AcceptedType {
        &self.accepted
    }

    pub(crate) fn resolve(
        &self,
        ctx: &EvalCtx<'_>,
        prospective: &Prospective,
    ) -> Result<ResolvedBlobPolicy, Rejection> {
        let mut stores = BTreeMap::new();
        let policy = self.policy.resolve(ctx, prospective, &mut stores)?;
        Ok(ResolvedBlobPolicy { policy, stores })
    }
}

/// A field's placement resolved against one final prospective state.
pub(crate) struct ResolvedBlobPolicy {
    pub(crate) policy: PlacementPolicy,
    pub(crate) stores: BTreeMap<StoreId, Store>,
}

#[derive(Clone)]
struct CompiledPolicy {
    plan: CompiledPlacement,
    serve: Option<TypedExpr>,
}

impl CompiledPolicy {
    fn resolve(
        &self,
        ctx: &EvalCtx<'_>,
        prospective: &Prospective,
        stores: &mut BTreeMap<StoreId, Store>,
    ) -> Result<PlacementPolicy, Rejection> {
        let plan = self.plan.resolve(ctx, prospective, stores)?;
        let serve = self
            .serve
            .as_ref()
            .map(|view| resolve_store_view(view, ctx, prospective, stores))
            .transpose()?;
        Ok(PlacementPolicy::new(plan, serve))
    }
}

#[derive(Clone)]
enum CompiledPlacement {
    View(TypedExpr),
    All(Vec<Self>),
    Any(Vec<Self>),
    Copies { n: usize, of: TypedExpr },
}

impl CompiledPlacement {
    fn resolve(
        &self,
        ctx: &EvalCtx<'_>,
        prospective: &Prospective,
        stores: &mut BTreeMap<StoreId, Store>,
    ) -> Result<Placement, Rejection> {
        match self {
            Self::View(view) => Ok(Placement::View(resolve_store_view(
                view,
                ctx,
                prospective,
                stores,
            )?)),
            Self::All(branches) => branches
                .iter()
                .map(|branch| branch.resolve(ctx, prospective, stores))
                .collect::<Result<Vec<_>, _>>()
                .map(Placement::All),
            Self::Any(branches) => branches
                .iter()
                .map(|branch| branch.resolve(ctx, prospective, stores))
                .collect::<Result<Vec<_>, _>>()
                .map(Placement::Any),
            Self::Copies { n, of } => Ok(Placement::Copies {
                n: *n,
                of: resolve_store_view(of, ctx, prospective, stores)?,
            }),
        }
    }
}

fn resolve_store_view(
    view: &TypedExpr,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    stores: &mut BTreeMap<StoreId, Store>,
) -> Result<Vec<StoreId>, Rejection> {
    let current = Cell::Row(Box::new(ctx.root(prospective)));
    let value = ctx.eval(prospective, view, &current)?;
    let rows = match value {
        Cell::Collection(rows) => rows,
        Cell::Row(row) => vec![*row],
        Cell::Scalar(_) => {
            return Err(Rejection::new(
                RejectionReason::Host,
                "a `$blob_storage` store view did not resolve to store rows",
            ));
        }
    };
    let mut ids = Vec::with_capacity(rows.len());
    for row in rows {
        let id = store_id(&row)?;
        let connector = text_cell(&row, "connector").ok_or_else(|| {
            Rejection::new(
                RejectionReason::Host,
                format!(
                    "blob store `{}` has no text `connector` field (§18.3)",
                    id.as_str()
                ),
            )
        })?;
        let enabled = match row.cell("enabled") {
            Some(Cell::Scalar(Value::Bool(enabled))) => *enabled,
            None => true,
            Some(_) => {
                return Err(Rejection::new(
                    RejectionReason::Host,
                    format!(
                        "blob store `{}` has a non-boolean `enabled` field",
                        id.as_str()
                    ),
                ));
            }
        };
        let store = Store {
            id: id.clone(),
            connector,
            enabled,
        };
        if let Some(previous) = stores.get(&id)
            && previous.connector != store.connector
        {
            return Err(Rejection::new(
                RejectionReason::Host,
                format!(
                    "blob store `{}` resolved to conflicting connectors",
                    id.as_str()
                ),
            ));
        }
        stores.insert(id.clone(), store);
        ids.push(id);
    }
    Ok(ids)
}

fn store_id(row: &liasse_expr::Row) -> Result<StoreId, Rejection> {
    match row.key() {
        Value::Text(id) => Ok(StoreId::new(id.as_str())),
        _ => text_cell(row, "id").map(StoreId::new).ok_or_else(|| {
            Rejection::new(
                RejectionReason::Host,
                "a blob store row must have a text identity (`$key: \"id\"`, `id: \"text\"`)",
            )
        }),
    }
}

fn text_cell(row: &liasse_expr::Row, name: &str) -> Option<String> {
    match row.cell(name) {
        Some(Cell::Scalar(Value::Text(text))) => Some(text.as_str().to_owned()),
        _ => None,
    }
}
