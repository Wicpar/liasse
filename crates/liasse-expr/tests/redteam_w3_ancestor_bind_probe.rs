#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! WAVE-3 finding: a gap in the §7.2 binding-qualified guard that the wave-2
//! fix left open — the guard checks the WRONG schema for an OUTER `::` binding.
//!
//! In `.entries::lines { $key: [acct], ... }` the projection body binds two rows
//! (§6.4): `lines` — the current source row, `== .` — and `entries`, the OUTER
//! ancestor row. `references_nonkey_field` (crates/liasse-expr/src/check/walk.rs
//! ~L139-147) treats EVERY `[:name]`/`::` bind in `row_binds` uniformly as "the
//! current source row" and checks `bind.field` against the *lines* source
//! schema. That is only correct for `lines`/an `[:it]` filter. For the ancestor
//! `entries`, the guard tests `entries.f` against the lines fields — an
//! unrelated schema — so its verdict depends on whether `f` happens to COINCIDE
//! with a lines field name:
//!
//!   `entries.id`     -> `id` IS a lines field (its key) and not the synthetic
//!                       key `acct`, so the guard REJECTS it.
//!   `entries.opened` -> `opened` is NOT a lines field, so the guard ACCEPTS it.
//!
//! Both are the same category of expression: an ancestor `::`-bind read of a
//! non-key field. §7.2 (verbatim): "group is the source-row view for that
//! output row. Every non-key source value MUST be aggregated or derived solely
//! from key values." A synthetic `$key` replaces the inherited chain identity
//! (§7.2 "a synthetic `$key` for grouping or a new identity"), so `entries` is
//! plain non-key source data. Under ANY single reading of §7.2 the two reads
//! MUST share one load verdict — either both rejected (they are non-key source
//! values) or both accepted (ancestor reads are outside the constraint). The
//! impl gives them OPPOSITE verdicts, decided by an irrelevant name
//! coincidence. That is impl != SPEC regardless of which reading is right.

mod common;

use common::{check_rejects, row_type, scalar, view, FixedScope};
use liasse_diag::SourceMap;
use liasse_expr::{check_statement, ExprType, Scope};
use liasse_syntax::parse_expression;
use liasse_value::Type;

/// `entries::lines`: entry has key `id`, a non-key `opened` (int), and nested
/// `lines`; a line has key `id`, non-key `account` (text), `debit` (int).
fn entries_scope() -> FixedScope {
    let line_ty = row_type(
        vec![
            ("id", scalar(Type::Text)),
            ("account", scalar(Type::Text)),
            ("debit", scalar(Type::Int)),
        ],
        Some(scalar(Type::Text)),
    );
    let entry_ty = row_type(
        vec![
            ("id", scalar(Type::Text)),
            ("opened", scalar(Type::Int)),
            ("lines", view(line_ty)),
        ],
        Some(scalar(Type::Text)),
    );
    let root_ty = row_type(vec![("entries", view(entry_ty))], None);
    FixedScope::new(ExprType::Row(root_ty))
}

/// Whether the checker ACCEPTS `source` (loads) — a non-panicking check, so both
/// verdicts can be compared in one assertion.
fn checks_ok(scope: &dyn Scope, source: &str) -> bool {
    let mut sources = SourceMap::new();
    let id = sources.add_label("test", source);
    let parsed = parse_expression(id, source)
        .unwrap_or_else(|d| panic!("parse failed:\n{}", d.render(&sources)));
    check_statement(scope, id, &parsed).is_ok()
}

/// CONTROL — the current-source-row read `lines.debit` (a non-key field of the
/// projected row) IS rejected by the guard, exactly as the wave-2 fix intends.
/// This isolates the defect to ancestor binds: current-source binds behave.
#[test]
fn control_current_source_bind_nonkey_read_is_rejected() {
    let scope = entries_scope();
    let diags = check_rejects(&scope, r#".entries::lines { $key: [acct], acct: account, x: lines.debit }"#);
    let rendered = format!("{diags:?}");
    assert!(
        rendered.contains("§7.2") || rendered.contains("neither"),
        "expected the §7.2 guard to reject `lines.debit`, got: {rendered}",
    );
}

/// FINDING — the guard's verdict on an ANCESTOR `::`-bind non-key read depends
/// on a coincidental field-name overlap with the current-source schema. Two
/// reads of the SAME category (`entries.id`, `entries.opened`) must load or
/// reject together; the impl splits them. FAILS today (`id` rejected, `opened`
/// accepted); passes once the guard resolves a bind against the row it actually
/// names rather than the current-source schema.
#[test]
fn ancestor_bind_nonkey_read_verdict_is_inconsistent() {
    let scope = entries_scope();
    let id_loads = checks_ok(&scope, r#".entries::lines { $key: [acct], acct: account, x: entries.id }"#);
    let opened_loads = checks_ok(&scope, r#".entries::lines { $key: [acct], acct: account, x: entries.opened }"#);
    assert_eq!(
        id_loads, opened_loads,
        "§7.2: `entries.id` and `entries.opened` are both ancestor `::`-bind non-key \
         source reads and MUST share one load verdict, but the guard checks the wrong \
         (current-source) schema: entries.id loads={id_loads}, entries.opened loads={opened_loads}",
    );
}
