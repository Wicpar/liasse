//! Driving the document grammar and lowering pest pairs into the spanned
//! [`SpannedDocument`] tree.

use liasse_diag::{ByteSpan, Diagnostic, Diagnostics, SourceId, Span};
use pest::Parser;
use pest::iterators::Pair;
use pest_derive::Parser;

use crate::clamp;
use crate::document::ast::{DocMember, DocName, DocValue, DocValueKind, SpannedDocument};
use crate::error::{Report, RuleLabel};
use crate::scan::{check_nesting_depth, Lexis};
use crate::text::RawString;

#[derive(Parser)]
#[grammar = "document/grammar.pest"]
struct DocGrammar;

/// Parses the authoring/built document form into a spanned tree.
///
/// `source` is the [`SourceId`] the caller registered `text` under in a
/// `liasse_diag::SourceMap`; it is used only to locate diagnostics.
pub fn parse_document(source: SourceId, text: &str) -> Result<SpannedDocument, Diagnostics> {
    // Reject pathologically nested input before pest's recursive descent can
    // overflow the stack (see `scan::check_nesting_depth`).
    check_nesting_depth(source, text, Lexis::Document)?;
    match DocGrammar::parse(Rule::document, text) {
        Ok(mut pairs) => {
            let mut builder = Builder {
                source,
                text,
                diags: Diagnostics::new(),
            };
            let Some(root) = pairs.next().and_then(|doc| builder.document(doc)) else {
                return Err(builder.aborted());
            };
            Ok(SpannedDocument { root })
        }
        Err(error) => Err(Report::new(source, text, Lexis::Document).build(error)),
    }
}

struct Builder<'s> {
    source: SourceId,
    text: &'s str,
    diags: Diagnostics,
}

impl Builder<'_> {
    fn document(&mut self, pair: Pair<'_, Rule>) -> Option<DocValue> {
        // `document = SOI ~ value ~ EOI`; the first inner pair is the value.
        let value = self.first_inner(&pair)?;
        self.value(value)
    }

    fn value(&mut self, pair: Pair<'_, Rule>) -> Option<DocValue> {
        let span = self.span(&pair);
        let inner = self.first_inner(&pair)?;
        let kind = match inner.as_rule() {
            Rule::object => DocValueKind::Object(self.object(inner)?),
            Rule::array => DocValueKind::Array(self.array(inner)?),
            Rule::multiline => DocValueKind::String(self.multiline(&inner)),
            Rule::string => DocValueKind::String(RawString::Quoted(inner.as_str()).decode()),
            Rule::boolean => DocValueKind::Bool(inner.as_str() == "true"),
            Rule::null_kw => DocValueKind::Null,
            Rule::number => DocValueKind::Number(inner.as_str().to_owned()),
            _ => return self.internal(span),
        };
        Some(DocValue { span, kind })
    }

    fn object(&mut self, pair: Pair<'_, Rule>) -> Option<Vec<DocMember>> {
        let mut members = Vec::new();
        for member in pair.into_inner() {
            if member.as_rule() == Rule::member {
                members.push(self.member(member)?);
            }
        }
        Some(members)
    }

    fn member(&mut self, pair: Pair<'_, Rule>) -> Option<DocMember> {
        let span = self.span(&pair);
        let mut inner = pair.into_inner();
        let name_pair = inner.next()?;
        let value_pair = inner.next()?;
        let name = self.name(name_pair)?;
        let value = self.value(value_pair)?;
        Some(DocMember { span, name, value })
    }

    fn name(&mut self, pair: Pair<'_, Rule>) -> Option<DocName> {
        let span = self.span(&pair);
        // `member_name = string | ident_name`.
        let inner = self.first_inner(&pair)?;
        let text = match inner.as_rule() {
            Rule::string => RawString::Quoted(inner.as_str()).decode(),
            Rule::ident_name => inner.as_str().to_owned(),
            _ => return self.internal(span),
        };
        Some(DocName { span, text })
    }

    fn array(&mut self, pair: Pair<'_, Rule>) -> Option<Vec<DocValue>> {
        let mut values = Vec::new();
        for value in pair.into_inner() {
            if value.as_rule() == Rule::value {
                values.push(self.value(value)?);
            }
        }
        Some(values)
    }

    fn multiline(&self, pair: &Pair<'_, Rule>) -> String {
        let gutter = self.gutter(pair.as_span().start());
        let body = pair
            .clone()
            .into_inner()
            .next()
            .map_or("", |body| body.as_str());
        RawString::Multiline { body, gutter }.decode()
    }

    /// Char column of `start` on its line, for multiline de-indentation.
    fn gutter(&self, start: usize) -> usize {
        let prefix = self.text.get(..start).unwrap_or("");
        let line_start = prefix.rfind('\n').map_or(0, |n| n + 1);
        self.text
            .get(line_start..start)
            .map_or(0, |s| s.chars().count())
    }

    fn first_inner<'p>(&mut self, pair: &Pair<'p, Rule>) -> Option<Pair<'p, Rule>> {
        let span = self.span(pair);
        match pair.clone().into_inner().next() {
            Some(inner) => Some(inner),
            None => {
                let _: Option<Vec<DocMember>> = self.internal(span);
                None
            }
        }
    }

    fn span(&self, pair: &Pair<'_, Rule>) -> ByteSpan {
        let span = pair.as_span();
        ByteSpan::cover(clamp(span.start()), clamp(span.end()))
    }

    /// Records an "internal parser error" and aborts. Reaching this means the
    /// grammar and the lowering disagree — a bug, not user input — but it must
    /// still not panic.
    fn internal<T>(&mut self, span: ByteSpan) -> Option<T> {
        self.diags.push(
            Diagnostic::error("internal parser error: unexpected grammar shape")
                .code("syntax-internal")
                .primary(Span::new(self.source, span), "while lowering this node")
                .build(),
        );
        None
    }

    fn aborted(mut self) -> Diagnostics {
        if self.diags.is_empty() {
            let span = ByteSpan::point(0);
            let _: Option<()> = self.internal(span);
        }
        self.diags
    }
}

impl RuleLabel for Rule {
    fn label(self) -> Option<&'static str> {
        Some(match self {
            Rule::object => "an object `{ ... }`",
            Rule::array => "an array `[ ... ]`",
            Rule::value => "a value",
            Rule::member => "a member",
            Rule::member_name => "a member name",
            Rule::string => "a quoted string",
            Rule::multiline => "a `'''` multiline string",
            Rule::number => "a number",
            Rule::boolean => "`true` or `false`",
            Rule::null_kw => "`null`",
            Rule::EOI => "end of input",
            _ => return None,
        })
    }
}
