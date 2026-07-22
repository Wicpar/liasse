//! Turning a raw pest parse failure into a [`Diagnostics`] batch. A pest error
//! string never reaches a user: its position becomes a real span, its expected
//! set becomes friendly prose, and a structural scan of the source adds a
//! fix hint (unclosed `{`/`[`/`(`, unterminated string) where one exists.

use liasse_diag::{ByteSpan, Diagnostic, Diagnostics, SourceId, Span};
use pest::error::{Error as PestError, ErrorVariant, InputLocation};

use crate::clamp;
use crate::scan::{Balance, DelimiterScan, Lexis};

/// A grammar rule that can name itself in a user-facing "expected ..." list.
/// Rules with no friendly name (internal fragments) return `None` and are
/// dropped from the message.
pub(crate) trait RuleLabel: Copy {
    /// A short, user-facing description of what this rule matches, if any.
    fn label(self) -> Option<&'static str>;
}

/// Maps one pest error onto diagnostics located in `source`.
pub(crate) struct Report<'s> {
    source: SourceId,
    text: &'s str,
    lexis: Lexis,
}

impl<'s> Report<'s> {
    pub(crate) fn new(source: SourceId, text: &'s str, lexis: Lexis) -> Self {
        Self {
            source,
            text,
            lexis,
        }
    }

    pub(crate) fn build<R: RuleLabel>(&self, error: PestError<R>) -> Diagnostics {
        let bytes = self.location(&error.location);
        let (headline, expected) = self.summarize(&error.variant);
        let primary_label = if self.at_end(bytes) {
            "unexpected end of input here".to_owned()
        } else {
            "unexpected token here".to_owned()
        };

        let head = Diagnostic::error(headline).code("syntax");
        let mut builder = head.primary(Span::new(self.source, bytes), primary_label);
        if let Some(expected) = expected {
            builder = builder.help(format!("expected {expected}"));
        }
        match DelimiterScan::of(self.text, self.lexis).balance() {
            Balance::Unclosed { at, opener } => {
                let opener_span = Span::new(self.source, ByteSpan::at(at, 1));
                builder = builder
                    .secondary(opener_span, format!("unclosed `{opener}` opened here"))
                    .help(format!("add a closing `{}`", Self::closer(opener)));
            }
            Balance::Unterminated { at } => {
                let quote_span = Span::new(self.source, ByteSpan::at(at, 1));
                builder = builder
                    .secondary(quote_span, "string opened here is never closed")
                    .help("add a closing quote to terminate the string");
            }
            Balance::Ok => {}
        }

        let mut diagnostics = Diagnostics::new();
        diagnostics.push(builder.build());
        diagnostics
    }

    fn location(&self, location: &InputLocation) -> ByteSpan {
        match *location {
            InputLocation::Pos(pos) => {
                let start = clamp(pos);
                // Point at one byte when there is one, else a caret at the end.
                if (start as usize) < self.text.len() {
                    ByteSpan::at(start, 1)
                } else {
                    ByteSpan::point(start)
                }
            }
            InputLocation::Span((start, end)) => {
                ByteSpan::cover(clamp(start), clamp(end))
            }
        }
    }

    fn at_end(&self, bytes: ByteSpan) -> bool {
        bytes.start() as usize >= self.text.len()
    }

    fn summarize<R: RuleLabel>(
        &self,
        variant: &ErrorVariant<R>,
    ) -> (String, Option<String>) {
        match variant {
            ErrorVariant::ParsingError {
                positives,
                negatives,
            } => {
                let expected = Self::friendly_list(positives);
                let headline = if expected.is_some() {
                    "unexpected token".to_owned()
                } else if !negatives.is_empty() {
                    "unexpected input".to_owned()
                } else {
                    "could not parse input".to_owned()
                };
                (headline, expected)
            }
            ErrorVariant::CustomError { message } => (message.clone(), None),
        }
    }

    fn friendly_list<R: RuleLabel>(rules: &[R]) -> Option<String> {
        let mut names: Vec<&'static str> = Vec::new();
        for rule in rules {
            if let Some(name) = rule.label()
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
        match names.as_slice() {
            [] => None,
            [only] => Some((*only).to_owned()),
            [head @ .., last] => Some(format!("{} or {last}", head.join(", "))),
        }
    }

    fn closer(opener: char) -> char {
        match opener {
            '{' => '}',
            '[' => ']',
            '(' => ')',
            '<' => '>',
            other => other,
        }
    }
}
