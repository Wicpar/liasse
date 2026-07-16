//! Typing of projection blocks (§7.1, §7.2, §7.3): output fields with acyclic
//! cross-references, synthetic `$key` grouping with the aggregate/key-derived
//! constraint, `$sort`, and `$skip`/`$limit` bounds.

use std::collections::BTreeSet;

use liasse_syntax::{BlockMember, BlockMemberKind, Expr, ExprKind};
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
        let (source_row, is_view) = match base.ty() {
            ExprType::View(row) => (row.clone(), true),
            ExprType::Row(row) => (row.clone(), false),
            other => {
                return self.error(expr, format!("cannot project a {}", other.describe()));
            }
        };

        let mut key_fields: Vec<String> = Vec::new();
        let mut raw_outputs: Vec<RawOutput> = Vec::new();
        let mut sort_members: Vec<&Expr> = Vec::new();
        let mut skip: Option<u64> = None;
        let mut limit: Option<u64> = None;

        for member in members {
            match &member.kind {
                BlockMemberKind::Directive { name, value } => {
                    match name.text.as_str() {
                        "key" => key_fields = self.key_field_names(value)?,
                        "sort" => sort_members = list_items(value),
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
                    raw_outputs.push(RawOutput {
                        name: shorthand_name(inner)?,
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
            let typed = match self.check(&raw.expr) {
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

        let sort = self.check_sort(&sort_members)?;
        self.pop_frame();

        let key = self.projected_key(&key_fields, &field_types, &source_row);
        let projected = RowType::new(field_types, key);
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

    fn check_sort(&mut self, members: &[&Expr]) -> Option<Vec<SortKey>> {
        let mut keys = Vec::with_capacity(members.len());
        for member in members {
            let (descending, key_expr) = match &member.kind {
                ExprKind::Unary { op: liasse_syntax::UnaryOp::Neg, operand } => (true, operand.as_ref()),
                _ => (false, *member),
            };
            let typed = self.check(key_expr)?;
            keys.push(SortKey { expr: typed, descending });
        }
        Some(keys)
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
