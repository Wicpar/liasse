//! Parsing the Liasse type-expression language (SPEC.md Annex A.2) into a
//! spanned type AST. Like the expression parser, a pest failure never reaches a
//! caller as a raw string: it becomes a located [`Diagnostics`] batch. The model
//! layer maps the resulting [`SpannedType`] to a canonical `liasse_value::Type`.

pub mod ast;

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
    // A struct type nests braces; guard the recursive descent as the other
    // grammars do (a type expression's own `#`/comment lexis matches Expression).
    check_nesting_depth(source, text, Lexis::Expression)?;
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
        Err(error) => Err(Report::new(source, text, Lexis::Expression).build(error)),
    }
}

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
            Rule::optional_type => TypeExprKind::Optional(Box::new(self.one_arg(inner)?)),
            Rule::set_type => TypeExprKind::Set(Box::new(self.one_arg(inner)?)),
            Rule::view_type => TypeExprKind::View(Box::new(self.one_arg(inner)?)),
            Rule::map_type => {
                let (key, value) = self.two_args(inner)?;
                TypeExprKind::Map(Box::new(key), Box::new(value))
            }
            Rule::ref_type => {
                let target = self.first_inner(&inner)?;
                TypeExprKind::Ref { target: target.as_str().to_owned() }
            }
            Rule::struct_type => TypeExprKind::Struct(self.struct_fields(inner)?),
            _ => return self.internal(span),
        };
        Some(SpannedType { span, kind })
    }

    fn one_arg(&mut self, pair: Pair<'_, Rule>) -> Option<SpannedType> {
        let arg = self.first_inner(&pair)?;
        self.type_expr(arg)
    }

    fn two_args(&mut self, pair: Pair<'_, Rule>) -> Option<(SpannedType, SpannedType)> {
        let mut inner = pair.into_inner();
        let key = self.type_expr(inner.next()?)?;
        let value = self.type_expr(inner.next()?)?;
        Some((key, value))
    }

    fn struct_fields(&mut self, pair: Pair<'_, Rule>) -> Option<Vec<TypeField>> {
        let mut fields = Vec::new();
        for field in pair.into_inner() {
            if field.as_rule() != Rule::struct_field {
                continue;
            }
            let span = self.span(&field);
            let mut parts = field.into_inner();
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
            fields.push(TypeField {
                name,
                name_span,
                optional,
                ty,
                span,
            });
        }
        Some(fields)
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
            Rule::struct_type => "a `{ field: type }` struct",
            Rule::struct_field => "a `name: type` field",
            Rule::named | Rule::field_name => "a type name",
            Rule::map_type => "a `map<K, V>`",
            Rule::optional_type | Rule::set_type | Rule::view_type => "a `wrapper<T>`",
            Rule::ref_type => "a `ref<target>`",
            Rule::key_path => "a `collection.$key` reference",
            Rule::EOI => "end of input",
            _ => return None,
        })
    }
}
