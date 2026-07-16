//! Type-expression parsing (SPEC.md Annex A.2): the spec's own type spellings
//! must lower to the expected spanned type AST, and malformed spellings must be
//! rejected with a located diagnostic.
//!
//! Tests return `Result` and route failures through `?` so the crate's
//! deny-by-default lints (no `unwrap`/`expect`/`panic!`/indexing) hold here too.

use liasse_diag::{Diagnostics, SourceMap};
use liasse_syntax::{parse_type_expression, SpannedType, TypeExprKind};

type Check = Result<(), String>;

fn parse_ok(text: &str) -> Result<SpannedType, String> {
    let mut sources = SourceMap::new();
    let id = sources.add_label("type", text);
    parse_type_expression(id, text).map_err(|d| d.render(&sources))
}

fn parse_err(text: &str) -> Result<Diagnostics, String> {
    let mut sources = SourceMap::new();
    let id = sources.add_label("type", text);
    match parse_type_expression(id, text) {
        Ok(_) => Err(format!("{text:?} parsed but a rejection was expected")),
        Err(diags) => Ok(diags),
    }
}

#[test]
fn nested_generic_type() -> Check {
    // `optional<map<text, json>>` — the §8.3 prototype example's type.
    let ty = parse_ok("optional<map<text, json>>")?;
    let TypeExprKind::Optional(inner) = ty.kind else {
        return Err(format!("expected optional<...>, got {:?}", ty.kind));
    };
    let TypeExprKind::Map(key, value) = inner.kind else {
        return Err(format!("expected map<...>, got {:?}", inner.kind));
    };
    assert_eq!(key.kind, TypeExprKind::Name("text".to_owned()));
    assert_eq!(value.kind, TypeExprKind::Name("json".to_owned()));
    Ok(())
}

#[test]
fn question_suffix_is_distinct_from_generic_optional() -> Check {
    // A.2: `T?` is shorthand for `optional<T>`; the AST keeps the two spellings
    // apart so the model can reject a redundant `optional<T>?`.
    let ty = parse_ok("text?")?;
    let TypeExprKind::OptionalSuffix(inner) = ty.kind else {
        return Err(format!("expected a `?` suffix, got {:?}", ty.kind));
    };
    assert_eq!(inner.kind, TypeExprKind::Name("text".to_owned()));
    Ok(())
}

#[test]
fn struct_type_with_optional_field() -> Check {
    // A.2: `{ field: T, optional_field?: U }`.
    let ty = parse_ok("{ line1: text, line2?: text }")?;
    let TypeExprKind::Struct(fields) = ty.kind else {
        return Err(format!("expected a struct type, got {:?}", ty.kind));
    };
    assert_eq!(fields.len(), 2);
    let (line1, line2) = (
        fields.first().ok_or("field 0")?,
        fields.get(1).ok_or("field 1")?,
    );
    assert_eq!(line1.name, "line1");
    assert!(!line1.optional);
    assert_eq!(line2.name, "line2");
    assert!(line2.optional);
    assert_eq!(line2.ty.kind, TypeExprKind::Name("text".to_owned()));
    Ok(())
}

#[test]
fn generic_keyword_prefix_still_parses_as_a_name() -> Check {
    // `setting` shares the `set` prefix but is a plain (possibly `$types`) name,
    // never `set<...>` — the grammar's ordered choice must fall through.
    let ty = parse_ok("setting")?;
    assert_eq!(ty.kind, TypeExprKind::Name("setting".to_owned()));
    Ok(())
}

#[test]
fn ref_and_key_path_forms() -> Check {
    // A.2 lists `ref<target>` and `collection.$key`; both are syntax here (the
    // model layer decides their standing).
    let ty = parse_ok("ref</companies>")?;
    let TypeExprKind::Ref { target } = ty.kind else {
        return Err(format!("expected ref<...>, got {:?}", ty.kind));
    };
    assert_eq!(target, "/companies");

    let key = parse_ok("orders.lines.$key")?;
    assert_eq!(key.kind, TypeExprKind::KeyPath("orders.lines.$key".to_owned()));
    Ok(())
}

#[test]
fn unclosed_generic_rejected_with_location() -> Check {
    let diags = parse_err("map<text, json")?;
    let diag = diags.iter().next().ok_or("expected one diagnostic")?;
    assert!(diag.is_error());
    // The caret lands where the `>` should be: at the end of the input.
    assert_eq!(diag.primary().span().bytes().start(), 14);
    Ok(())
}

#[test]
fn dangling_question_rejected() -> Check {
    // A `?` needs a base type before it.
    let diags = parse_err("?text")?;
    assert!(diags.has_errors());
    Ok(())
}
