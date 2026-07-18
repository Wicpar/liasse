//! Typing of projection blocks (§7.1, §7.2, §7.3): output fields with acyclic
//! cross-references, synthetic `$key` grouping with the aggregate/key-derived
//! constraint, `$sort`, and `$skip`/`$limit` bounds.

use std::collections::BTreeSet;

use liasse_diag::ByteSpan;
use liasse_syntax::{parse_expression, BlockMember, BlockMemberKind, Expr, ExprKind, Ident, StmtKind};
use liasse_value::{StructType, Type};

use crate::check::Checker;
use crate::check::walk::{
    list_items, member_self, order_outputs, references_nonkey_field, shorthand_name, RawOutput,
};
use crate::ty::{ExprType, RowType};
use crate::typed::{Output, Projection, SortKey, TypedExpr, TypedKind};

impl Checker<'_> {
    pub(crate) fn check_block(
        &mut self,
        expr: &Expr,
        base: &Expr,
        members: &[BlockMember],
    ) -> Option<TypedExpr> {
        let base = self.check(base)?;
        // §6.3: a projection carries the cardinality of the row source it maps
        // over. A base that already denotes a single row — a lone scalar or
        // composite key selection (`check_select` types it `Row`, per §6.3 line
        // 700: "one scalar or composite key … one row when it exists") — projects
        // to that one `Row`, which a one-row-requiring context (a single-row
        // `return`, a scalar row value, a mutation receiver) renders as an object
        // and which `eval_select` rejects when zero or several rows occur (§6.3
        // line 702). A base that denotes a view — a filter (`[:name | …]`), a
        // multi-key or set/ref selection, or a declared collection view — projects
        // to a `View`, an ordinary multi-row result. This mirrors §8 (line 1014):
        // an insertion of exactly one row returns that row (object) while a
        // multi-row view returns the view (array). The source's own `ExprType`
        // already encodes the distinction, so the projection simply propagates it.
        let (source_row, is_view) = match base.ty() {
            ExprType::View(row) => (row.clone(), true),
            ExprType::Row(row) => (row.clone(), false),
            other => {
                return self.error(expr, format!("cannot project a {}", other.describe()));
            }
        };
        // §14.5: a projection over an unbounded recurring bucket does NOT reject
        // here. Like a `Select` (filter, `check_select`), a projection is
        // transparent to unbounded-ness — it reshapes WHICH fields each row carries
        // without forcing enumeration on its own — so the flag PROPAGATES onto the
        // projected result row (set on `projected` below) exactly as `check_select`
        // copies it through. An enclosing bounded temporal selector then clears it
        // (`check_temporal_call`, `unbounded(false)`), which is what lets
        // `.bucket { proj }.$at(t)` load — matching the filter-first
        // `.bucket[:x|p].$at(t)`, the stored-bucket parity, and the evaluator's
        // `rebase_scopes` `Project` arm (§7.1: a projected bucketed base still names
        // the collection the selector addresses; §14.1: `.$at` bounds the read).
        //
        // The §14.5 enumeration guard is deferred to where enumeration is GENUINELY
        // forced and no bounding selector cleared the flag: the terminal expression
        // result (`check_expression`), an aggregate over the whole series
        // (`check_aggregate`), and `.$all` (`check_temporal_all`).

        let mut key_fields: Vec<String> = Vec::new();
        let mut raw_outputs: Vec<RawOutput> = Vec::new();
        let mut sort_members: Vec<&Expr> = Vec::new();
        let mut quantity_member: Option<&Expr> = None;
        let mut skip: Option<u64> = None;
        let mut limit: Option<u64> = None;

        for member in members {
            match &member.kind {
                BlockMemberKind::Directive { name, value } => {
                    match name.text.as_str() {
                        "key" => key_fields = self.key_field_names(value)?,
                        "sort" => sort_members = list_items(value),
                        "quantity" => quantity_member = Some(value),
                        "skip" => skip = Some(self.bound(value)?),
                        "limit" => limit = Some(self.bound(value)?),
                        other => {
                            return self.error(expr, format!("unknown projection directive `${other}`"));
                        }
                    }
                }
                BlockMemberKind::Named { name, value: Some(value) } => {
                    raw_outputs.push(RawOutput { name: name.text.clone(), expr: value.clone() });
                }
                BlockMemberKind::Named { name, value: None } => {
                    raw_outputs.push(RawOutput {
                        name: name.text.clone(),
                        expr: member_self(member),
                    });
                }
                BlockMemberKind::Shorthand(inner) => {
                    // §7.1: a shorthand output takes its name from the projected
                    // member (`title` / `.title` / `@title`). Any other
                    // expression has no derivable output name and must be given
                    // one explicitly.
                    let Some(name) = shorthand_name(inner) else {
                        return self.error(
                            inner,
                            "this projection output has no name; write it as `name: expression` (§7.1)",
                        );
                    };
                    raw_outputs.push(RawOutput {
                        name,
                        expr: inner.clone(),
                    });
                }
                BlockMemberKind::Clear(_) | BlockMemberKind::Assign { .. } => {
                    return self.error(expr, "a patch member is not a projection output");
                }
            }
        }

        let grouped = !key_fields.is_empty();
        self.push_frame(ExprType::Row(source_row.clone()));
        let mut binds = Vec::new();
        // §6.4: every `[:name]` binding and `::`/`.`-traversed level along the
        // selection chain stays visible in the projection body, so the outputs
        // may read `name.field`. Eval threads these through `RowScope`; the
        // checker mirrors the whole chain here.
        Self::traverse_binds(&base, &mut binds);
        for (name, ty) in binds {
            self.bind(name, ty);
        }
        if grouped {
            self.bind("group".to_owned(), ExprType::View(source_row.clone()));
        }

        let order = match order_outputs(&raw_outputs) {
            Some(order) => order,
            None => {
                self.pop_frame();
                return self.error(expr, "projection outputs form a dependency cycle (§7.1)");
            }
        };

        let key_set: BTreeSet<&str> = key_fields.iter().map(String::as_str).collect();
        let mut outputs: Vec<Output> = Vec::with_capacity(raw_outputs.len());
        let mut field_types: Vec<(String, ExprType)> = Vec::new();
        for index in order {
            let raw = match raw_outputs.get(index) {
                Some(raw) => raw,
                None => continue,
            };
            if grouped
                && !key_set.contains(raw.name.as_str())
                && references_nonkey_field(&raw.expr, &source_row, &key_set)
            {
                self.pop_frame();
                return self.error(
                    &raw.expr,
                    format!(
                        "grouped output `{}` is a non-key source value that is neither \
                         aggregated nor derived from key values (§7.2)",
                        raw.name
                    ),
                );
            }
            // §7.1 `nested: { ... }`: a bare object output projects the
            // same-named source struct or nested collection. `.` inside the
            // block is that field, so rewrite it to `.name { ... }` and let the
            // ordinary projection machinery type (and later evaluate) it.
            let nested;
            let to_check = match &raw.expr.kind {
                ExprKind::Object(members)
                    if matches!(
                        source_row.field(&raw.name),
                        Some(ExprType::Row(_) | ExprType::View(_))
                    ) =>
                {
                    nested = nested_projection(&raw.name, members, raw.expr.span);
                    &nested
                }
                _ => &raw.expr,
            };
            let typed = match self.check(to_check) {
                Some(typed) => typed,
                None => {
                    self.pop_frame();
                    return None;
                }
            };
            self.bind(raw.name.clone(), typed.ty().clone());
            field_types.push((raw.name.clone(), typed.ty().clone()));
            outputs.push(Output { name: raw.name.clone(), expr: typed });
        }

        let quantity = self.check_quantity(quantity_member)?;
        if let Some(quantity) = &quantity {
            field_types.push(("$quantity".to_owned(), quantity.ty().clone()));
        }
        let sort = self.check_sort(&sort_members)?;
        self.pop_frame();

        let key = self.projected_key(&key_fields, &field_types, &source_row);
        // §14.5: carry the source's unbounded-recurring marker onto the reshaped
        // row so it propagates up like a `Select` does — a bounding temporal
        // selector clears it, a terminal read is rejected downstream.
        let projected = RowType::new(field_types, key).unbounded(source_row.is_unbounded());
        let result_ty = if is_view {
            ExprType::View(projected)
        } else {
            ExprType::Row(projected)
        };
        Some(TypedExpr::new(
            expr.span,
            result_ty.clone(),
            TypedKind::Project {
                source: Box::new(base),
                projection: Projection {
                    key: key_fields,
                    outputs,
                    quantity: quantity.map(Box::new),
                    sort,
                    skip,
                    limit,
                },
            },
        ))
    }

    /// `$key` is a scalar output name or an array of names (§7.2).
    fn key_field_names(&mut self, value: &Expr) -> Option<Vec<String>> {
        match &value.kind {
            ExprKind::Name(name) => Some(vec![name.text.clone()]),
            ExprKind::List(items) => {
                let mut names = Vec::with_capacity(items.len());
                for item in items {
                    match &item.kind {
                        ExprKind::Name(name) => names.push(name.text.clone()),
                        _ => {
                            self.report(item, "a `$key` component names an output field");
                            return None;
                        }
                    }
                }
                Some(names)
            }
            _ => {
                self.report(value, "`$key` is an output name or an array of names");
                None
            }
        }
    }

    fn bound(&mut self, value: &Expr) -> Option<u64> {
        let typed = self.check(value)?;
        match typed.kind() {
            TypedKind::Literal(v) => {
                match v.to_canonical_json_string().trim_matches('"').parse::<u64>() {
                    Ok(n) => Some(n),
                    Err(_) => {
                        self.report(value, "`$skip`/`$limit` must be a non-negative integer (§7.3)");
                        None
                    }
                }
            }
            // A `-n` literal is a negated integer constant: reject as negative.
            TypedKind::Neg { .. } => {
                self.report(value, "`$skip`/`$limit` must be a non-negative integer (§7.3)");
                None
            }
            _ => {
                self.report(value, "`$skip`/`$limit` must be a constant integer");
                None
            }
        }
    }

    /// Type the `$quantity` pool-capacity directive (§15.1). It is evaluated over
    /// the source row like an output, so it is checked inside the pushed frame.
    /// A pool capacity is an exact numeric quantity (`int`/`decimal`); other
    /// types are rejected. Non-negativity is a runtime admission check, not a
    /// static one (SPEC-ISSUES item 13).
    fn check_quantity(&mut self, member: Option<&Expr>) -> Option<Option<TypedExpr>> {
        let Some(value) = member else {
            return Some(None);
        };
        let typed = self.check(value)?;
        match typed.ty().as_scalar() {
            Some(Type::Int | Type::Decimal) => Some(Some(typed)),
            _ => {
                self.report(
                    value,
                    "`$quantity` is a pool capacity and must be a numeric (`int`/`decimal`) value (§15.1)",
                );
                None
            }
        }
    }

    fn check_sort(&mut self, members: &[&Expr]) -> Option<Vec<SortKey>> {
        let mut keys = Vec::with_capacity(members.len());
        for member in members {
            let key = match &member.kind {
                // §7.3 structured form: `{ $by: field, $dir: asc|desc }` — the
                // same ordering choice the string form spells with a leading `-`.
                ExprKind::Object(entry) => self.structured_sort_key(member, entry)?,
                // §7.3 canonical wire form: a `$sort` entry is a *string* holding
                // the key expression, optionally prefixed by `-` for descending
                // (Annex B: `["-created_at", "id"]`, `["string.casefold(name)",
                // "name"]`). The string content is a sort-key expression, not a
                // constant, so re-parse and check it in the projection frame.
                ExprKind::Str(text) => {
                    let (descending, body) = match text.strip_prefix('-') {
                        Some(rest) => (true, rest),
                        None => (false, text.as_str()),
                    };
                    SortKey { expr: self.parse_sort_key(member, body)?, descending }
                }
                // §7.3 compact DSL form: a leading `-` reverses one key; a bare
                // key ascends.
                ExprKind::Unary { op: liasse_syntax::UnaryOp::Neg, operand } => SortKey {
                    expr: self.check(operand)?,
                    descending: true,
                },
                _ => SortKey {
                    expr: self.check(member)?,
                    descending: false,
                },
            };
            keys.push(key);
        }
        Some(keys)
    }

    /// A structured `$sort` entry `{ $by: field, $dir: asc|desc }` (§7.3). `$by`
    /// is the comparison key expression (checked like a string-form key); `$dir`
    /// is `asc` (default) or `desc`. It denotes the same order as `field` /
    /// `-field`.
    fn structured_sort_key(&mut self, member: &Expr, entry: &[BlockMember]) -> Option<SortKey> {
        let mut by: Option<&Expr> = None;
        let mut descending = false;
        for part in entry {
            let BlockMemberKind::Directive { name, value } = &part.kind else {
                self.report(member, "a structured `$sort` entry is `{ $by: field, $dir: asc|desc }`");
                return None;
            };
            match name.text.as_str() {
                "by" => by = Some(value),
                "dir" => descending = self.sort_direction(value)?,
                other => {
                    self.report(member, format!("a `$sort` entry has `$by` and `$dir`, not `${other}`"));
                    return None;
                }
            }
        }
        let Some(by) = by else {
            self.report(member, "a structured `$sort` entry needs a `$by` key (§7.3)");
            return None;
        };
        // `$by` may be spelled as a bare key expression (compact DSL) or, in the
        // canonical wire form, as a string holding that expression (`$by: "name"`,
        // `$by: "string.casefold(name)"`). A string is the key expression, not a
        // text constant.
        let expr = match &by.kind {
            ExprKind::Str(text) => self.parse_sort_key(by, text)?,
            _ => self.check(by)?,
        };
        Some(SortKey { expr, descending })
    }

    /// Parse the expression a `$sort` string entry carries and check it in the
    /// current projection frame (so a bare column name resolves to the projected
    /// output, §7.3). The canonical wire form spells every sort key as a string;
    /// its content is a full sort-key expression, never a text literal.
    fn parse_sort_key(&mut self, member: &Expr, body: &str) -> Option<TypedExpr> {
        let parsed = match parse_expression(self.source, body) {
            Ok(parsed) => parsed,
            Err(_) => {
                self.report(
                    member,
                    "a `$sort` string entry must hold a sort-key expression (§7.3)",
                );
                return None;
            }
        };
        match &parsed.statement().kind {
            StmtKind::Bare(inner) | StmtKind::Return(inner) => {
                let inner = inner.clone();
                self.check(&inner)
            }
            _ => {
                self.report(
                    member,
                    "a `$sort` string entry must hold a sort-key expression (§7.3)",
                );
                None
            }
        }
    }

    /// A `$dir` value: `asc` (ascending, the default) or `desc` (descending).
    fn sort_direction(&mut self, value: &Expr) -> Option<bool> {
        // The bare `asc`/`desc` token parses as a name; the spec's quoted
        // spelling parses as a text literal. Accept both.
        let word = match &value.kind {
            ExprKind::Name(name) => Some(name.text.as_str()),
            ExprKind::Str(text) => Some(text.as_str()),
            _ => None,
        };
        match word {
            Some("asc") => Some(false),
            Some("desc") => Some(true),
            _ => {
                self.report(value, "`$dir` is `asc` or `desc` (§7.3)");
                None
            }
        }
    }

    fn projected_key(
        &self,
        key_fields: &[String],
        field_types: &[(String, ExprType)],
        source_row: &RowType,
    ) -> Option<ExprType> {
        if key_fields.is_empty() {
            return source_row.key().cloned();
        }
        let lookup = |name: &str| {
            field_types
                .iter()
                .find(|(n, _)| n == name)
                .and_then(|(_, ty)| ty.as_scalar())
                .cloned()
        };
        match key_fields {
            [single] => lookup(single).map(ExprType::scalar),
            many => {
                let components: Vec<(String, Type)> = many
                    .iter()
                    .filter_map(|name| lookup(name).map(|ty| (name.clone(), ty)))
                    .collect();
                Some(ExprType::scalar(Type::Struct(StructType::new(components))))
            }
        }
    }
}

/// Desugar a nested projection output `name: { members }` into `.name { members }`
/// (§7.1): a `Block` whose base is the same-named field of the current source
/// row, so `.` inside the block is that struct/collection.
fn nested_projection(name: &str, members: &[BlockMember], span: ByteSpan) -> Expr {
    let field = Expr {
        span,
        kind: ExprKind::Field {
            base: Box::new(Expr::current(span)),
            member: Ident {
                span,
                text: name.to_owned(),
                structural: false,
            },
        },
    };
    Expr {
        span,
        kind: ExprKind::Block {
            base: Box::new(field),
            members: members.to_vec(),
        },
    }
}
