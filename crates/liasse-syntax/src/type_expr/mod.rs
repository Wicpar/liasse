//! Parsing the Liasse type-expression language (SPEC.md Annex A.2) into a
//! spanned type AST. Like the expression parser, a pest failure never reaches a
//! caller as a raw string: it becomes a located [`Diagnostics`] batch. The model
//! layer maps the resulting [`SpannedType`] to a canonical `liasse_value::Type`.

pub mod ast;
mod object;

use liasse_diag::{ByteSpan, Diagnostic, Diagnostics, SourceId, Span};
use pest::Parser;
use pest::iterators::Pair;
use pest_derive::Parser;

use crate::clamp;
use crate::error::{Report, RuleLabel};
use crate::scan::{check_nesting_depth, Lexis};

use ast::{SpannedType, TypeExprKind, TypeField};

#[derive(Parser)]
#[grammar = "type_expr/grammar.pest"]
struct TypeGrammar;

/// Parse one A.2 type expression into a spanned type AST.
///
/// `source` is the [`SourceId`] the caller registered `text` under in a
/// `liasse_diag::SourceMap`; it is used only to locate diagnostics.
pub fn parse_type_expression(source: SourceId, text: &str) -> Result<SpannedType, Diagnostics> {
    // An object type nests braces, and the removed parametric spellings (kept
    // only to be rejected by name) nest angle brackets; both drive the recursive
    // descent here and the model's recursive type lowering, so guard the depth
    // before either runs. The `Type` lexis counts `<`/`>` alongside `{` for
    // exactly this reason.
    check_nesting_depth(source, text, Lexis::Type)?;
    match TypeGrammar::parse(Rule::type_program, text) {
        Ok(mut pairs) => {
            let mut builder = TypeBuilder {
                source,
                diags: Diagnostics::new(),
            };
            let node = pairs
                .next()
                .and_then(|program| builder.first_inner(&program))
                .and_then(|expr| builder.type_expr(expr));
            match node {
                Some(node) => Ok(node),
                None => Err(builder.aborted()),
            }
        }
        Err(error) => Err(Report::new(source, text, Lexis::Type).build(error)),
    }
}

/// Lowers the pest tree to a [`SpannedType`], accumulating every rejection.
/// The object-type classification and the removed-spelling rejections live in
/// [`object`], which continues this impl.
struct TypeBuilder {
    source: SourceId,
    diags: Diagnostics,
}

impl TypeBuilder {
    fn type_expr(&mut self, pair: Pair<'_, Rule>) -> Option<SpannedType> {
        let span = self.span(&pair);
        let mut inner = pair.into_inner();
        let base = self.base(inner.next()?)?;
        // An `optional_suffix` after the base wraps it (A.2 `T?`).
        match inner.next() {
            Some(suffix) if suffix.as_rule() == Rule::optional_suffix => Some(SpannedType {
                span,
                kind: TypeExprKind::OptionalSuffix(Box::new(base)),
            }),
            Some(other) => {
                let span = self.span(&other);
                self.internal(span)
            }
            None => Some(base),
        }
    }

    fn base(&mut self, pair: Pair<'_, Rule>) -> Option<SpannedType> {
        let span = self.span(&pair);
        let inner = self.first_inner(&pair)?;
        let kind = match inner.as_rule() {
            Rule::named => TypeExprKind::Name(inner.as_str().to_owned()),
            Rule::key_path => TypeExprKind::KeyPath(inner.as_str().to_owned()),
            Rule::object_type => self.object_type(inner)?,
            Rule::legacy_generic => self.reject_legacy(&inner)?,
            _ => return self.internal(span),
        };
        Some(SpannedType { span, kind })
    }

    /// One `field: T` / `field?: T` member of an object type.
    fn struct_field(&mut self, pair: Pair<'_, Rule>) -> Option<TypeField> {
        let span = self.span(&pair);
        let mut parts = pair.into_inner();
        let name_pair = parts.next()?;
        let name_span = self.span(&name_pair);
        let name = name_pair.as_str().to_owned();
        let mut optional = false;
        let mut ty_pair = parts.next()?;
        if ty_pair.as_rule() == Rule::optional_suffix {
            optional = true;
            ty_pair = parts.next()?;
        }
        let ty = self.type_expr(ty_pair)?;
        Some(TypeField {
            name,
            name_span,
            optional,
            ty,
            span,
        })
    }

    fn first_inner<'p>(&mut self, pair: &Pair<'p, Rule>) -> Option<Pair<'p, Rule>> {
        let span = self.span(pair);
        match pair.clone().into_inner().next() {
            Some(inner) => Some(inner),
            None => {
                let _: Option<()> = self.internal(span);
                None
            }
        }
    }

    fn span(&self, pair: &Pair<'_, Rule>) -> ByteSpan {
        let span = pair.as_span();
        ByteSpan::cover(clamp(span.start()), clamp(span.end()))
    }

    fn internal<T>(&mut self, span: ByteSpan) -> Option<T> {
        self.diags.push(
            Diagnostic::error("internal parser error: unexpected type-expression shape")
                .code("syntax-internal")
                .primary(Span::new(self.source, span), "while lowering this node")
                .build(),
        );
        None
    }

    fn aborted(mut self) -> Diagnostics {
        if self.diags.is_empty() {
            let _: Option<()> = self.internal(ByteSpan::point(0));
        }
        self.diags
    }
}

impl RuleLabel for Rule {
    fn label(self) -> Option<&'static str> {
        Some(match self {
            Rule::type_program | Rule::type_expr | Rule::base => "a type expression",
            Rule::object_type => "a `{ ... }` object type",
            Rule::object_member => "an object-type member",
            Rule::struct_field => "a `name: type` field",
            Rule::typed_marker => "a `$marker: type` member",
            Rule::ref_marker => "a `$ref: target` member",
            Rule::marker_name | Rule::ref_key => "a `$` type marker",
            Rule::named | Rule::field_name => "a type name",
            Rule::ref_target => "a target path",
            Rule::key_path => "a `collection.$key` reference",
            Rule::EOI => "end of input",
            _ => return None,
        })
    }
}
