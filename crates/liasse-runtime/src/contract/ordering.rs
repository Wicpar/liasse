//! Explicit-ordering normalization for the Annex E boundary check (E.2/E.5).
//!
//! A `$public`/role view's declared `$sort` is a boundary contract: a same-major
//! forward release MUST preserve it (E.5 "changing explicit sort semantics"). This
//! module reads a view text's top-level projection `$sort` and normalizes it to an
//! ordered `(key, descending)` sequence so the three §7.3 spellings — string
//! (`"-name"`), compact (`-name`), and structured (`{ $by: name, $dir: desc }`) —
//! of the same ordering compare equal. A view that is not a plain top-level
//! projection whose ordering the check can read yields `None` (a combinator or
//! nested source is a documented seam, left uncompared so a compatible release is
//! never mis-flagged).

use liasse_diag::SourceMap;
use liasse_syntax::{parse_expression, BlockMember, BlockMemberKind, DocValue, Expr, ExprKind, StmtKind, UnaryOp};

use crate::doc;

/// The exposed explicit ordering of a surface view (E.2/E.5): the ordered
/// `(key, descending)` sequence its top-level projection `$sort` declares, or an
/// empty sequence when the projection declares no `$sort`. `None` when the `$view`
/// is not a plain top-level projection whose ordering the check can read.
pub(super) fn view_ordering(decl: &DocValue) -> Option<Vec<(String, bool)>> {
    let text = doc::member(decl, "$view").and_then(doc::string)?;
    let mut sources = SourceMap::new();
    let source = sources.add_label("compat-ordering", text.to_owned());
    let parsed = parse_expression(source, text).ok()?;
    let StmtKind::Bare(expr) = &parsed.statement().kind else {
        return None;
    };
    let ExprKind::Block { members, .. } = &expr.kind else {
        return None;
    };
    for member in members {
        if let BlockMemberKind::Directive { name, value } = &member.kind
            && name.text == "sort"
        {
            return sort_keys(value, text);
        }
    }
    Some(Vec::new())
}

/// Normalize a projection `$sort` directive value — a §7.3 array of comparison
/// keys — into the ordered `(key, descending)` sequence. `None` when the value
/// is not the expected array form (a malformed `$sort` is left uncompared rather
/// than mis-flagged).
fn sort_keys(value: &Expr, text: &str) -> Option<Vec<(String, bool)>> {
    let ExprKind::List(items) = &value.kind else {
        return None;
    };
    items.iter().map(|item| sort_key(item, text)).collect()
}

/// Normalize one §7.3 sort key into `(key, descending)`. The three spellings all
/// reduce to the same pair: a string `"-field"`, a compact `-field`, and a
/// structured `{ $by: field, $dir: desc }` each yield `("field", true)`.
fn sort_key(item: &Expr, text: &str) -> Option<(String, bool)> {
    match &item.kind {
        // Canonical wire form: the string holds the key expression, a leading `-`
        // reversing it (§7.3).
        ExprKind::Str(spelling) => {
            let spelling = spelling.trim();
            match spelling.strip_prefix('-') {
                Some(body) => Some((body.trim().to_owned(), true)),
                None => Some((spelling.to_owned(), false)),
            }
        }
        // Compact DSL: a leading `-` reverses one key.
        ExprKind::Unary { op: UnaryOp::Neg, operand } => Some((key_text(operand, text)?, true)),
        // Structured form: `{ $by: field, $dir: asc|desc }`.
        ExprKind::Object(members) => structured_sort_key(members, text),
        // A bare ascending key expression.
        _ => Some((key_text(item, text)?, false)),
    }
}

/// Normalize a structured §7.3 sort key `{ $by: field, $dir: asc|desc }` into
/// `(key, descending)`. `None` when `$by` is absent.
fn structured_sort_key(members: &[BlockMember], text: &str) -> Option<(String, bool)> {
    let mut by = None;
    let mut descending = false;
    for member in members {
        let BlockMemberKind::Directive { name, value } = &member.kind else {
            continue;
        };
        match name.text.as_str() {
            "by" => {
                by = Some(match &value.kind {
                    ExprKind::Str(spelling) => spelling.trim().to_owned(),
                    _ => key_text(value, text)?,
                });
            }
            "dir" => {
                if let ExprKind::Str(spelling) = &value.kind {
                    descending = spelling.trim() == "desc";
                }
            }
            _ => {}
        }
    }
    Some((by?, descending))
}

/// The source text of a bare (non-string) sort-key expression, so a compact
/// `-field` or structured `$by: field` key compares equal to its string
/// spelling `"field"`. Outer whitespace is trimmed; inner spelling is compared
/// verbatim.
fn key_text(expr: &Expr, text: &str) -> Option<String> {
    let start = expr.span.start() as usize;
    let end = expr.span.end() as usize;
    text.get(start..end).map(|slice| slice.trim().to_owned())
}
