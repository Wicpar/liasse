//! Expression parsing: representative constructs from the spec's own examples
//! must lower to the expected spanned AST, and malformed expressions must be
//! rejected with a located diagnostic.
//!
//! Tests return `Result` and route failures through `?` so the crate's
//! deny-by-default lints (no `unwrap`/`expect`/`panic!`/indexing) hold here too.

use liasse_diag::{Diagnostics, SourceMap};
use liasse_syntax::{
    parse_expression, Arg, BinaryOp, BlockMemberKind, CombinatorOp, Expr, ExprKind, Selector,
    SpannedExpression, StmtKind, UnaryOp,
};

type Check = Result<(), String>;

fn parse_ok(text: &str) -> Result<SpannedExpression, String> {
    let mut sources = SourceMap::new();
    let id = sources.add_label("expr", text);
    parse_expression(id, text).map_err(|d| d.render(&sources))
}

fn parse_err(text: &str) -> Result<Diagnostics, String> {
    let mut sources = SourceMap::new();
    let id = sources.add_label("expr", text);
    match parse_expression(id, text) {
        Ok(_) => Err(format!("{text:?} parsed but a rejection was expected")),
        Err(diags) => Ok(diags),
    }
}

/// The bare expression of a single-expression program.
fn bare(text: &str) -> Result<Expr, String> {
    match parse_ok(text)?.statement.kind {
        StmtKind::Bare(expr) => Ok(expr),
        other => Err(format!("expected a bare expression, got {other:?}")),
    }
}

#[test]
fn field_arithmetic_shape_and_spans() -> Check {
    // `.subtotal + .tax` — the §5.2 computed-field body.
    let expr = bare(".subtotal + .tax")?;
    let ExprKind::Binary { op, lhs, rhs } = expr.kind else {
        return Err(format!("expected a binary op, got {:?}", expr.kind));
    };
    assert_eq!(op, BinaryOp::Add);
    // `.subtotal` occupies bytes 0..9, `.tax` occupies 12..16.
    assert_eq!((lhs.span.start(), lhs.span.end()), (0, 9));
    assert_eq!((rhs.span.start(), rhs.span.end()), (12, 16));
    let ExprKind::Field { base, member } = lhs.kind else {
        return Err(format!("expected field access, got {:?}", lhs.kind));
    };
    assert_eq!(base.kind, ExprKind::Current);
    assert_eq!(member.text, "subtotal");
    Ok(())
}

#[test]
fn bound_selector_with_filter() -> Check {
    // `.tasks[:task | !task.done]` — §6.4 row binding with a predicate.
    let expr = bare(".tasks[:task | !task.done]")?;
    let ExprKind::Select { base, selector } = expr.kind else {
        return Err(format!("expected a selector, got {:?}", expr.kind));
    };
    assert!(matches!(base.kind, ExprKind::Field { .. }));
    let Selector::Bind { name, condition } = selector else {
        return Err("expected a bound selector".to_owned());
    };
    assert_eq!(name.text, "task");
    let cond = condition.ok_or("expected a filter condition")?;
    assert!(matches!(
        cond.kind,
        ExprKind::Unary {
            op: UnaryOp::Not,
            ..
        }
    ));
    Ok(())
}

#[test]
fn projection_block_member_forms() -> Check {
    // §7.1 projection: a bare field, an explicit output, and a `$sort` directive.
    let expr = bare(".people { first, full: first, $sort: [-created_at] }")?;
    let ExprKind::Block { members, .. } = expr.kind else {
        return Err(format!("expected a projection block, got {:?}", expr.kind));
    };
    assert_eq!(members.len(), 3);
    let (first, second, third) = (
        members.first().ok_or("member 0")?,
        members.get(1).ok_or("member 1")?,
        members.get(2).ok_or("member 2")?,
    );
    assert!(matches!(first.kind, BlockMemberKind::Shorthand(_)));
    assert!(matches!(
        second.kind,
        BlockMemberKind::Named { value: Some(_), .. }
    ));
    let BlockMemberKind::Directive { name, .. } = &third.kind else {
        return Err(format!("expected a $sort directive, got {:?}", third.kind));
    };
    assert_eq!(name.text, "sort");
    assert!(name.structural);
    Ok(())
}

#[test]
fn row_method_call_chain() -> Check {
    // `.tasks[@id].complete()` — §8.2 row mutation call.
    let expr = bare(".tasks[@id].complete()")?;
    let ExprKind::Call { callee, args } = expr.kind else {
        return Err(format!("expected a call, got {:?}", expr.kind));
    };
    assert!(args.is_empty(), "complete() takes no arguments");
    let ExprKind::Field { base, member } = callee.kind else {
        return Err("expected the callee to be a field access".to_owned());
    };
    assert_eq!(member.text, "complete");
    assert!(matches!(base.kind, ExprKind::Select { .. }));
    Ok(())
}

#[test]
fn namespace_function_call() -> Check {
    // `string.trim(.)` — §6.5 built-in namespace call.
    let expr = bare("string.trim(.)")?;
    let ExprKind::Call { callee, args } = expr.kind else {
        return Err("expected a call".to_owned());
    };
    let ExprKind::Field { base, member } = callee.kind else {
        return Err("expected namespace.fn field access".to_owned());
    };
    assert_eq!(member.text, "trim");
    let ExprKind::Name(id) = base.kind else {
        return Err(format!("expected the namespace name, got {:?}", base.kind));
    };
    assert_eq!(id.text, "string");
    assert_eq!(args.len(), 1);
    assert!(matches!(
        args.first(),
        Some(Arg::Positional(Expr {
            kind: ExprKind::Current,
            ..
        }))
    ));
    Ok(())
}

#[test]
fn ternary_with_empty_view_branch() -> Check {
    // `has(#billing) ? #billing.customers : []` — §7.4 conditional view.
    let expr = bare("has(#billing) ? #billing.customers : []")?;
    let ExprKind::Ternary {
        cond, otherwise, ..
    } = expr.kind
    else {
        return Err(format!("expected a ternary, got {:?}", expr.kind));
    };
    assert!(matches!(cond.kind, ExprKind::Call { .. }));
    // The empty view `[]` is an empty list literal.
    assert_eq!(otherwise.kind, ExprKind::List(Vec::new()));
    Ok(())
}

#[test]
fn mixed_combinator_chain_stays_flat() -> Check {
    // SPEC-ISSUES item 25: the spec pins no precedence for `|` vs `&`, so a
    // mixed chain is recorded flat rather than nested into an arbitrary tree.
    let expr = bare(".a | .b & .c")?;
    let ExprKind::Combination {
        operands,
        operators,
    } = expr.kind
    else {
        return Err(format!("expected a flat combination, got {:?}", expr.kind));
    };
    assert_eq!(operands.len(), 3);
    assert_eq!(operators, vec![CombinatorOp::Union, CombinatorOp::Intersect]);
    Ok(())
}

#[test]
fn return_statement() -> Check {
    // Annex C.9 final statement.
    let stmt = parse_ok("return . { id, title }")?.statement;
    let StmtKind::Return(expr) = stmt.kind else {
        return Err(format!("expected a return statement, got {:?}", stmt.kind));
    };
    assert!(matches!(expr.kind, ExprKind::Block { .. }));
    Ok(())
}

#[test]
fn return_of_named_projection() -> Check {
    // SPEC.md §3.2 uses `return task { id, title }` as a mutation's final
    // statement. The `return` keyword must be recognised even when a bare
    // identifier (not `.`/`/`) starts the returned expression: the atomic
    // `return_kw` enforces the word boundary before whitespace is skipped.
    let stmt = parse_ok("return task { id, title }")?.statement;
    let StmtKind::Return(expr) = stmt.kind else {
        return Err(format!("expected a return statement, got {:?}", stmt.kind));
    };
    // The returned expression is `task { id, title }`: a projection block whose
    // base is the bare name `task`.
    let ExprKind::Block { base, members } = expr.kind else {
        return Err(format!("expected a projection block, got {:?}", expr.kind));
    };
    let ExprKind::Name(name) = &base.kind else {
        return Err(format!("expected the base to be the name `task`, got {:?}", base.kind));
    };
    assert_eq!(name.text, "task");
    assert_eq!(members.len(), 2);
    Ok(())
}

#[test]
fn returned_is_an_identifier_not_a_return() -> Check {
    // `returned` shares the `return` prefix but continues with `ident_cont`
    // characters, so the word boundary fails and the whole token is one name.
    let expr = bare("returned")?;
    let ExprKind::Name(name) = &expr.kind else {
        return Err(format!("expected a bare name, got {:?}", expr.kind));
    };
    assert_eq!(name.text, "returned");
    Ok(())
}

#[test]
fn returnx_is_an_identifier_not_a_return() -> Check {
    // `returnx` likewise fails the `return` word boundary and parses as a name,
    // never as `return x`.
    let expr = bare("returnx")?;
    let ExprKind::Name(name) = &expr.kind else {
        return Err(format!("expected a bare name, got {:?}", expr.kind));
    };
    assert_eq!(name.text, "returnx");
    Ok(())
}

#[test]
fn assignment_statement() -> Check {
    // `.done = true` — a mutation assignment (single `=`, not `==`).
    let stmt = parse_ok(".done = true")?.statement;
    let StmtKind::Assign { target, value } = stmt.kind else {
        return Err(format!("expected an assignment, got {:?}", stmt.kind));
    };
    assert!(matches!(target.kind, ExprKind::Field { .. }));
    assert_eq!(value.kind, ExprKind::Bool(true));
    Ok(())
}

#[test]
fn patch_block_shorthand_and_clear() -> Check {
    // §8.6 patch block: `@title` sets from a parameter, `-note` clears a field.
    let expr = bare(".tasks[@id] { @title, -note }")?;
    let ExprKind::Block { members, .. } = expr.kind else {
        return Err(format!("expected a patch block, got {:?}", expr.kind));
    };
    assert_eq!(members.len(), 2);
    match members.first().map(|m| &m.kind) {
        Some(BlockMemberKind::Shorthand(Expr {
            kind: ExprKind::Param(id),
            ..
        })) => assert_eq!(id.text, "title"),
        other => return Err(format!("expected @title shorthand, got {other:?}")),
    }
    match members.get(1).map(|m| &m.kind) {
        Some(BlockMemberKind::Clear(id)) => assert_eq!(id.text, "note"),
        other => return Err(format!("expected -note clear, got {other:?}")),
    }
    Ok(())
}

#[test]
fn parent_and_structural_roots() -> Check {
    // `^^.plan` reaches two lexical scopes up (§6.2).
    let expr = bare("^^.plan")?;
    let ExprKind::Field { base, member } = expr.kind else {
        return Err("expected field access on a parent scope".to_owned());
    };
    assert_eq!(base.kind, ExprKind::Parent(2));
    assert_eq!(member.text, "plan");

    // `$actor.$key` — a structural root whose accessed member is also `$`-named.
    let expr = bare("$actor.$key")?;
    let ExprKind::Field { base, member } = expr.kind else {
        return Err("expected field access on a structural root".to_owned());
    };
    let ExprKind::Structural(id) = base.kind else {
        return Err(format!("expected $actor, got {:?}", base.kind));
    };
    assert_eq!(id.text, "actor");
    assert_eq!(member.text, "key");
    assert!(member.structural, "$key is a structural member");
    Ok(())
}

#[test]
fn composite_key_selector() -> Check {
    // §6.3 composite-key lookup uses one object operand.
    let expr = bare(".tax_rates[{ country: @country, code: @code }]")?;
    let ExprKind::Select { selector, .. } = expr.kind else {
        return Err("expected a selector".to_owned());
    };
    let Selector::Keys(keys) = selector else {
        return Err("expected a key-list selector".to_owned());
    };
    assert_eq!(keys.len(), 1);
    assert!(matches!(
        keys.first().map(|k| &k.kind),
        Some(ExprKind::Object(_))
    ));
    Ok(())
}

#[test]
fn same_name_traversal() -> Check {
    // §6.4 `::` binds each traversed collection to its own name.
    let expr = bare(".projects::tasks")?;
    let ExprKind::SameName { base, member } = expr.kind else {
        return Err(format!("expected a same-name traversal, got {:?}", expr.kind));
    };
    assert_eq!(member.text, "tasks");
    assert!(matches!(base.kind, ExprKind::Field { .. }));
    Ok(())
}

#[test]
fn decimal_and_integer_literals_are_distinguished() -> Check {
    assert_eq!(bare("42")?.kind, ExprKind::Int("42".to_owned()));
    assert_eq!(bare("1.50")?.kind, ExprKind::Decimal("1.50".to_owned()));
    Ok(())
}

#[test]
fn empty_expression_is_rejected() -> Check {
    // SPEC-ISSUES item 25 (bare `=`): an empty expression body is a hard error,
    // not a silent empty node. `parse_expression("")` is the stripped form of a
    // `$data` value of `"="`.
    let diags = parse_err("")?;
    assert!(diags.has_errors());
    let diag = diags.iter().next().ok_or("expected one diagnostic")?;
    assert!(diag
        .helps()
        .iter()
        .any(|h| h.contains("statement or expression")));
    Ok(())
}

#[test]
fn unclosed_block_after_import_sigil_keeps_its_hint() -> Check {
    // `#` is the import sigil in expression source, not a comment: an unclosed
    // `{` following `#name` must still surface a closing-brace fix hint. (In the
    // document form `#` opens a line comment, so the structural scanner must not
    // treat this expression's `#billing` as a comment that swallows the `{`.)
    let diags = parse_err("#billing { customers")?;
    let diag = diags.iter().next().ok_or("expected one diagnostic")?;
    assert!(
        diag.helps().iter().any(|h| h.contains("closing `}`")),
        "expected a closing-brace hint, got {:?}",
        diag.helps()
    );
    let opener = diag
        .secondaries()
        .iter()
        .find(|l| l.message().contains("unclosed"))
        .ok_or("expected an opener label")?;
    // The `{` opens at byte 9, past the `#billing ` prefix.
    assert_eq!(opener.span().bytes().start(), 9);
    Ok(())
}

#[test]
fn empty_selector_brackets_are_rejected() -> Check {
    // `red/empty-selector-brackets-invalid`: a selector needs a key or binding.
    let diags = parse_err(".items[]")?;
    let diag = diags.iter().next().ok_or("expected one diagnostic")?;
    // The caret sits at the `]`, byte offset 7.
    assert_eq!(diag.primary().span().bytes().start(), 7);
    assert!(diag.helps().iter().any(|h| h.contains("key or `:binding`")));
    Ok(())
}

#[test]
fn deeply_nested_parens_are_rejected_before_the_parser_recurses() -> Check {
    // 60k nested `(` overflows pest's recursive descent (SIGABRT). The pre-parse
    // nesting guard rejects it with a located diagnostic and does not crash.
    let deep = "(".repeat(60_000);
    let diags = parse_err(&deep)?;
    let diag = diags.iter().next().ok_or("expected one diagnostic")?;
    assert!(
        diag.message().contains("512"),
        "the message must state the 512 limit, got {:?}",
        diag.message()
    );
    // The caret points at the first paren past the cap: the 513th `(`, byte 512.
    assert_eq!(diag.primary().span().bytes().start(), 512);
    assert!(
        diag.helps().iter().any(|h| h.contains("nest")),
        "expected a restructuring hint, got {:?}",
        diag.helps()
    );
    Ok(())
}

#[test]
fn grouping_just_under_the_cap_is_accepted() -> Check {
    // Depth 511 nested groups around `1`: below the 512 cap, so the guard passes
    // it and pest parses it (`grouped` unwraps, so the value is the integer 1).
    //
    // The expression grammar descends through its whole precedence chain per
    // parenthesis, so 511 groups amplify to several thousand pest frames — more
    // than the test harness's small default thread stack. We run the parse on a
    // generously sized thread so the test measures the *guard threshold* (511 is
    // accepted), not the harness's stack default. The 512 cap keeps depth
    // bounded and documented; a caller that parses near it provisions stack to
    // match.
    let src = format!("{}1{}", "(".repeat(511), ")".repeat(511));
    let handle = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(move || bare(&src).map(|expr| expr.kind))
        .map_err(|e| e.to_string())?;
    let kind = handle.join().map_err(|_| "parser thread panicked".to_owned())??;
    assert_eq!(kind, ExprKind::Int("1".to_owned()));
    Ok(())
}
