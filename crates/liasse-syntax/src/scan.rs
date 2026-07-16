//! Pre-parse structural scan of source text, shared by the delimiter-balance
//! error hint and the nesting-depth guard.
//!
//! One left-to-right pass records the ordered brackets, skipping comments and
//! string bodies (the comment lexis differs per surface). From that single scan
//! a caller can find the first unmatched opener (for a fix hint after the
//! grammar rejects) or the first opener that breaches the nesting cap (rejected
//! *before* the grammar runs) — without re-walking the text or duplicating the
//! skip rules.

use liasse_diag::{ByteSpan, Diagnostic, Diagnostics, SourceId, Span};

use crate::clamp;

/// The maximum bracket-nesting depth accepted before the grammar runs.
///
/// The Liasse spec pins no nesting limit — Annex C fixes no depth bound — so
/// this cap is an implementation safeguard, not a spec rule. `pest`'s recursive
/// descent overflows the stack (SIGABRT) on pathologically nested input; this
/// prescan rejects such input with a diagnostic before a single grammar rule
/// fires. 512 clears every real Liasse document and expression by a wide margin
/// while staying far under the stack budget.
pub(crate) const MAX_NESTING_DEPTH: usize = 512;

/// Which surface's comment lexis the scanner assumes. The two surfaces differ
/// only in `#`: it opens a line comment in the document form (Hjson), but is the
/// import sigil `#name` in expression source, so scanning an expression with the
/// document rule would swallow the rest of the line and lose a bracket.
#[derive(Clone, Copy)]
pub(crate) enum Lexis {
    /// The Hjson document form: `//`, `#`, and `/* */` all begin comments.
    Document,
    /// Expression source: only `//` and `/* */` begin comments; `#` is a sigil.
    Expression,
}

impl Lexis {
    fn hash_is_comment(self) -> bool {
        matches!(self, Self::Document)
    }
}

/// The earliest structural imbalance a [`DelimiterScan`] reveals.
pub(crate) enum Balance {
    Ok,
    Unclosed { at: u32, opener: char },
    Unterminated { at: u32 },
}

/// One structural bracket: an opener carries its character so an unclosed one
/// can name itself; a closer needs no more than its position.
enum Bracket {
    Open(char),
    Close,
}

/// The ordered structural brackets of a source text, with comments and string
/// bodies excluded. Built once, queried for both balance and nesting depth.
pub(crate) struct DelimiterScan {
    brackets: Vec<(Bracket, u32)>,
    unterminated: Option<u32>,
}

impl DelimiterScan {
    /// Scan `text` once, recording every structural bracket outside comments and
    /// strings. Stops at the first unterminated string, whose offset is kept.
    pub(crate) fn of(text: &str, lexis: Lexis) -> Self {
        let mut brackets = Vec::new();
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let rest = text.get(i..).unwrap_or("");
            if rest.starts_with("//") || (lexis.hash_is_comment() && rest.starts_with('#')) {
                i += Self::skip_line(rest);
                continue;
            }
            if rest.starts_with("/*") {
                i += Self::skip_block(rest);
                continue;
            }
            if rest.starts_with("'''") {
                match Self::skip_multiline(rest) {
                    Some(len) => i += len,
                    None => return Self { brackets, unterminated: Some(clamp(i)) },
                }
                continue;
            }
            let c = match text.get(i..).and_then(|s| s.chars().next()) {
                Some(c) => c,
                None => break,
            };
            let width = c.len_utf8();
            match c {
                '"' | '\'' => match Self::skip_string(rest, c) {
                    Some(len) => {
                        i += len;
                        continue;
                    }
                    None => return Self { brackets, unterminated: Some(clamp(i)) },
                },
                '{' | '[' | '(' => brackets.push((Bracket::Open(c), clamp(i))),
                '}' | ']' | ')' => brackets.push((Bracket::Close, clamp(i))),
                _ => {}
            }
            i += width;
        }
        Self {
            brackets,
            unterminated: None,
        }
    }

    /// The earliest unmatched opener or unterminated string, or [`Balance::Ok`].
    /// An unterminated string wins over an earlier unclosed opener, matching the
    /// order in which the scan gives up.
    pub(crate) fn balance(&self) -> Balance {
        if let Some(at) = self.unterminated {
            return Balance::Unterminated { at };
        }
        let mut stack: Vec<(char, u32)> = Vec::new();
        for (bracket, at) in &self.brackets {
            match bracket {
                Bracket::Open(opener) => stack.push((*opener, *at)),
                Bracket::Close => {
                    let _ = stack.pop();
                }
            }
        }
        match stack.first() {
            Some(&(opener, at)) => Balance::Unclosed { at, opener },
            None => Balance::Ok,
        }
    }

    /// The byte offset of the first opener whose nesting depth exceeds `limit`,
    /// or `None` when the input never nests that deep. Unmatched closers cannot
    /// drive the depth negative (`saturating_sub`), so a closer-heavy prefix
    /// never masks a later run of openers.
    pub(crate) fn depth_breach(&self, limit: usize) -> Option<u32> {
        let mut depth: usize = 0;
        for (bracket, at) in &self.brackets {
            match bracket {
                Bracket::Open(_) => {
                    depth += 1;
                    if depth > limit {
                        return Some(*at);
                    }
                }
                Bracket::Close => depth = depth.saturating_sub(1),
            }
        }
        None
    }

    fn skip_line(rest: &str) -> usize {
        rest.find('\n').map_or(rest.len(), |n| n + 1)
    }

    fn skip_block(rest: &str) -> usize {
        rest.get(2..)
            .and_then(|body| body.find("*/"))
            .map_or(rest.len(), |n| n + 4)
    }

    fn skip_multiline(rest: &str) -> Option<usize> {
        rest.get(3..)
            .and_then(|body| body.find("'''"))
            .map(|n| n + 6)
    }

    /// Length of a quoted string starting at `rest[0] == quote`, or `None` if it
    /// runs off the end. Respects `\` escapes.
    fn skip_string(rest: &str, quote: char) -> Option<usize> {
        let mut escaped = false;
        for (offset, c) in rest.char_indices().skip(1) {
            if escaped {
                escaped = false;
                continue;
            }
            if c == '\\' {
                escaped = true;
                continue;
            }
            if c == quote {
                return Some(offset + c.len_utf8());
            }
        }
        None
    }
}

/// Reject input whose brackets nest past [`MAX_NESTING_DEPTH`], before the
/// grammar runs, with a diagnostic pointing at the offending opener. This keeps
/// `pest`'s recursive descent from overflowing the stack on adversarial input.
pub(crate) fn check_nesting_depth(
    source: SourceId,
    text: &str,
    lexis: Lexis,
) -> Result<(), Diagnostics> {
    let Some(at) = DelimiterScan::of(text, lexis).depth_breach(MAX_NESTING_DEPTH) else {
        return Ok(());
    };
    let diagnostic = Diagnostic::error(format!(
        "input nests brackets more than {MAX_NESTING_DEPTH} levels deep"
    ))
    .code("syntax")
    .primary(
        Span::new(source, ByteSpan::at(at, 1)),
        "this opening bracket exceeds the maximum nesting depth",
    )
    .help(format!(
        "restructure the input so brackets nest no more than {MAX_NESTING_DEPTH} levels deep"
    ))
    .build();
    let mut diagnostics = Diagnostics::new();
    diagnostics.push(diagnostic);
    Err(diagnostics)
}
