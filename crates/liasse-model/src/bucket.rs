//! Buckets: lifecycle and source-backed period collections (SPEC.md §14, C.13).
//!
//! A `$bucket` is either the short form — one until-expression — or the object
//! form of `$source?`/`$from?`/`$until?`/`$repeat?`. This validates that shape
//! and types the interval expressions in scope:
//!
//! * `$from` is a `timestamp`, `$until` an optional `timestamp`, `$repeat` an
//!   optional `period` (§14.2, §14.5);
//! * a lifecycle bucket's expressions read the collection row as `.`; a
//!   source-backed bucket exposes the source row through the `$source`
//!   structural binding (§14.4), plus `$created`/`$from`/`$until`/`$index`.
//!
//! CORE scope: a source-backed bucket's interval expressions are typed only when
//! `$source` itself types cleanly; unbounded-recurring enumeration (§14.5) and
//! inferred identity (§14.6) are runtime seams. The read-only rule for
//! source-backed rows (§14.4) is enforced in the mutation phase via the
//! recorded source-bucket paths.

use liasse_diag::SourceMap;
use liasse_expr::{check_statement, ExprType};
use liasse_syntax::parse_expression;
use liasse_value::Type;

use crate::build::RawDecl;
use crate::doc::DocValueExt;
use crate::report::{code, Reporter};
use crate::resolve::{row_at, Resolver};
use crate::scope::ModelScope;
use crate::state::Shape;

/// Validate every `$bucket` declaration.
pub(crate) fn check(
    reporter: &mut Reporter,
    sources: &mut SourceMap,
    resolver: &Resolver,
    root: &Shape,
    buckets: &[RawDecl],
) {
    let root_row = ExprType::Row(resolver.shape_row(root));
    for bucket in buckets {
        let mut phase = BucketPhase {
            reporter,
            sources,
            root_row: root_row.clone(),
        };
        phase.check_bucket(bucket);
    }
}

struct BucketPhase<'a, 'b> {
    reporter: &'a mut Reporter<'b>,
    sources: &'a mut SourceMap,
    root_row: ExprType,
}

impl BucketPhase<'_, '_> {
    fn check_bucket(&mut self, bucket: &RawDecl) {
        let receiver = row_at(&self.root_row, &bucket.path).unwrap_or_else(|| self.root_row.clone());
        // The parent row is the `.` context a `$source` view is read against.
        let parent_path = bucket.path.split_last().map(|(_, rest)| rest).unwrap_or(&[]);
        let parent = row_at(&self.root_row, parent_path).unwrap_or_else(|| self.root_row.clone());

        // Short form: one until-expression against the collection row.
        if let Some(text) = bucket.value.as_string() {
            let scope = self.lifecycle_scope(&receiver);
            self.expect_timestamp(&scope, text, bucket.span, "$until", true);
            return;
        }
        let Some(members) = bucket.value.as_object() else {
            self.reporter.reject_hint(
                bucket.span,
                code::BUCKET,
                "`$bucket` is an until-expression or an object of `$source`/`$from`/`$until`/`$repeat`",
                "e.g. `\"$bucket\": \".expires_at\"`",
            );
            return;
        };
        self.check_object(members, &receiver, &parent, bucket.span);
    }

    fn check_object(
        &mut self,
        members: &[liasse_syntax::DocMember],
        receiver: &ExprType,
        parent: &ExprType,
        span: liasse_diag::ByteSpan,
    ) {
        // Bind `$source` when a source view is declared and types cleanly.
        let source_row = members
            .iter()
            .find(|m| m.name.text == "$source")
            .and_then(|m| self.source_row(&m.value, parent));
        let scope = self.bucket_scope(receiver, source_row.as_ref());

        for member in members {
            match member.name.text.as_str() {
                "$source" => {}
                "$from" => self.expect_timestamp(&scope, expr_text(&member.value), member.value.span, "$from", false),
                "$until" => self.expect_timestamp(&scope, expr_text(&member.value), member.value.span, "$until", true),
                "$repeat" => self.expect_period(&scope, expr_text(&member.value), member.value.span),
                other => self.reporter.reject(
                    member.span,
                    code::BUCKET,
                    format!("`{other}` is not a `$bucket` member"),
                ),
            }
        }
        let _ = span;
    }

    /// Type the `$source` view against the parent row; its row shape binds
    /// `$source` for the interval expressions.
    fn source_row(&mut self, value: &liasse_syntax::DocValue, parent: &ExprType) -> Option<ExprType> {
        let text = value.as_string()?;
        let scope = ModelScope::nested(vec![parent.clone()], self.root_row.clone());
        let typed = self.type_expr(&scope, text, value.span)?;
        typed.ty().as_view().map(|row| ExprType::Row(row.clone()))
    }

    fn lifecycle_scope(&self, receiver: &ExprType) -> ModelScope {
        ModelScope::nested(vec![receiver.clone()], self.root_row.clone())
            .with_structural("created", ExprType::scalar(Type::timestamp()))
    }

    fn bucket_scope(&self, receiver: &ExprType, source: Option<&ExprType>) -> ModelScope {
        let mut scope = self.lifecycle_scope(receiver);
        if let Some(source) = source {
            scope = scope.with_structural("source", source.clone());
        }
        scope
            .with_structural("from", ExprType::scalar(Type::timestamp()))
            .with_structural("until", ExprType::scalar(Type::Optional(Box::new(Type::timestamp()))))
            .with_structural("index", ExprType::scalar(Type::Int))
    }

    fn expect_timestamp(&mut self, scope: &ModelScope, text: &str, span: liasse_diag::ByteSpan, member: &str, optional: bool) {
        let Some(typed) = self.type_expr(scope, text, span) else {
            return;
        };
        let Some(scalar) = typed.ty().as_scalar() else {
            self.reporter.reject(span, code::BUCKET, format!("`{member}` must be a timestamp expression"));
            return;
        };
        let base = match scalar {
            Type::Optional(inner) => inner.as_ref(),
            other => other,
        };
        let is_optional = matches!(scalar, Type::Optional(_));
        if !matches!(base, Type::Timestamp(_)) || (is_optional && !optional) {
            self.reporter.reject_hint(
                span,
                code::BUCKET,
                format!("`{member}` has type `{}` but a{} timestamp is required", scalar.name(), if optional { "n optional" } else { "" }),
                "produce a timestamp interval bound",
            );
        }
    }

    fn expect_period(&mut self, scope: &ModelScope, text: &str, span: liasse_diag::ByteSpan) {
        let Some(typed) = self.type_expr(scope, text, span) else {
            return;
        };
        let base = match typed.ty().as_scalar() {
            Some(Type::Optional(inner)) => Some(inner.as_ref()),
            other => other,
        };
        if !matches!(base, Some(Type::Period)) {
            self.reporter.reject_hint(
                span,
                code::BUCKET,
                "`$repeat` must be an optional period expression",
                "select a `period?` value, e.g. `/plans[$source.plan].period`",
            );
        }
    }

    fn type_expr(&mut self, scope: &ModelScope, text: &str, span: liasse_diag::ByteSpan) -> Option<liasse_expr::TypedExpr> {
        if text.trim().is_empty() {
            self.reporter.reject(span, code::BUCKET, "a `$bucket` expression must not be empty");
            return None;
        }
        let sub = self.sources.add_label("bucket", text.to_owned());
        let parsed = match parse_expression(sub, text) {
            Ok(parsed) => parsed,
            Err(diags) => {
                self.reporter.emit_all(diags);
                return None;
            }
        };
        let sub = self.sources.add_label("bucket", text.to_owned());
        match check_statement(scope, sub, &parsed) {
            Ok(typed) => Some(typed),
            Err(diags) => {
                self.reporter.emit_all(diags);
                None
            }
        }
    }
}

fn expr_text(value: &liasse_syntax::DocValue) -> &str {
    value.as_string().unwrap_or_default()
}
