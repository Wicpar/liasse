//! Type-expression parsing (SPEC.md Annex A.2): the spec's own type spellings
//! must lower to the expected spanned type AST, and malformed spellings must be
//! rejected with a located diagnostic.
//!
//! A.2 has one type syntax — a keyword, a name, a `?` suffix, and an object
//! bearing exactly one kind marker for every composite. The removed parametric
//! spellings (`T?`, `{ $set: T }`, `{ $key: K, $value: V }`, `view<T>`, `ref<target>`)
//! must be rejected by name, never silently accepted or re-read.
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

/// The first diagnostic's message plus its helps, for message assertions.
fn rejection(text: &str) -> Result<String, String> {
    let diags = parse_err(text)?;
    let diag = diags.iter().next().ok_or("expected one diagnostic")?;
    if !diag.is_error() {
        return Err(format!("{text:?} produced a non-error diagnostic"));
    }
    Ok(format!("{} | {}", diag.message(), diag.helps().join(" ")))
}

#[test]
fn nested_object_type() -> Check {
    // A.2 / §8.3: the prototype example's type, `{ $key: text, $value: json }?`.
    let ty = parse_ok("{ $key: text, $value: json }?")?;
    let TypeExprKind::OptionalSuffix(inner) = ty.kind else {
        return Err(format!("expected a `?` suffix, got {:?}", ty.kind));
    };
    let TypeExprKind::Map(key, value) = inner.kind else {
        return Err(format!("expected a map type, got {:?}", inner.kind));
    };
    assert_eq!(key.kind, TypeExprKind::Name("text".to_owned()));
    assert_eq!(value.kind, TypeExprKind::Name("json".to_owned()));
    Ok(())
}

#[test]
fn map_markers_are_order_independent() -> Check {
    // The markers name their roles, so declaration order carries no meaning.
    let ty = parse_ok("{ $value: json, $key: text }")?;
    let TypeExprKind::Map(key, value) = ty.kind else {
        return Err(format!("expected a map type, got {:?}", ty.kind));
    };
    assert_eq!(key.kind, TypeExprKind::Name("text".to_owned()));
    assert_eq!(value.kind, TypeExprKind::Name("json".to_owned()));
    Ok(())
}

#[test]
fn set_and_view_object_forms() -> Check {
    // A.2: `{ $set: T }` and `{ $view: T }` — at a type location `$view` carries
    // a type, where a declaration's `$view` carries an expression.
    let set = parse_ok("{ $set: text }")?;
    let TypeExprKind::Set(inner) = set.kind else {
        return Err(format!("expected a set type, got {:?}", set.kind));
    };
    assert_eq!(inner.kind, TypeExprKind::Name("text".to_owned()));

    let view = parse_ok("{ $view: { id: uuid } }")?;
    let TypeExprKind::View(inner) = view.kind else {
        return Err(format!("expected a view type, got {:?}", view.kind));
    };
    let TypeExprKind::Struct(fields) = inner.kind else {
        return Err("expected the view's row to be a struct type".to_owned());
    };
    assert_eq!(fields.len(), 1);
    Ok(())
}

#[test]
fn question_suffix_is_the_only_optional_spelling() -> Check {
    // A.2: optionality is `T?` at a type location and `field?: T` in an object.
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
fn removed_constructor_keyword_still_parses_as_a_name() -> Check {
    // `setting` shares the `set` prefix but is a plain (possibly `$types`) name —
    // the grammar's ordered choice must fall through to `named`.
    let ty = parse_ok("setting")?;
    assert_eq!(ty.kind, TypeExprKind::Name("setting".to_owned()));
    let ty = parse_ok("mapper")?;
    assert_eq!(ty.kind, TypeExprKind::Name("mapper".to_owned()));
    Ok(())
}

#[test]
fn ref_and_key_path_forms() -> Check {
    // A.2 lists `{ $ref: target }` and `collection.$key`; both are syntax here
    // (the model layer decides their standing).
    let ty = parse_ok("{ $ref: /companies }")?;
    let TypeExprKind::Ref { target } = ty.kind else {
        return Err(format!("expected a ref type, got {:?}", ty.kind));
    };
    assert_eq!(target, "/companies");

    let key = parse_ok("orders.lines.$key")?;
    assert_eq!(key.kind, TypeExprKind::KeyPath("orders.lines.$key".to_owned()));
    Ok(())
}

#[test]
fn every_removed_parametric_spelling_is_rejected_by_name() -> Check {
    // A.2: no parametric `<>` type form survives. Each removed constructor is
    // named in its own rejection, together with the object form that replaces it
    // — never a silent fallback to some other reading.
    for (source, ctor, replacement) in [
        ("optional<text>", "optional", "`?` suffix"),
        ("set<text>", "set", "{ $set: T }"),
        ("view<text>", "view", "{ $view: T }"),
        ("map<text, json>", "map", "{ $key: K, $value: V }"),
        ("ref</companies>", "ref", "{ $ref: target }"),
    ] {
        let rendered = rejection(source)?;
        assert!(
            rendered.contains(&format!("`{ctor}<…>`")),
            "{source}: the rejection must name the removed constructor, got {rendered:?}"
        );
        assert!(
            rendered.contains(replacement),
            "{source}: the rejection must point at {replacement}, got {rendered:?}"
        );
    }
    Ok(())
}

#[test]
fn a_removed_spelling_nested_in_an_object_is_still_rejected() -> Check {
    // The rejection is not confined to the outermost node: a legacy spelling
    // anywhere in the tree fails the whole type expression.
    let rendered = rejection("{ $set: optional<text> }")?;
    assert!(rendered.contains("`optional<…>`"), "got {rendered:?}");
    Ok(())
}

#[test]
fn map_needs_both_of_its_markers() -> Check {
    let key_only = rejection("{ $key: text }")?;
    assert!(key_only.contains("$value"), "got {key_only:?}");
    let value_only = rejection("{ $value: json }")?;
    assert!(value_only.contains("$key"), "got {value_only:?}");
    Ok(())
}

#[test]
fn there_is_no_optional_marker() -> Check {
    // A.2: optionality has exactly two spellings and neither is a marker.
    let rendered = rejection("{ $optional: text }")?;
    assert!(rendered.contains("$optional"), "got {rendered:?}");
    assert!(rendered.contains('?'), "the help must offer the `?` suffix, got {rendered:?}");
    Ok(())
}

#[test]
fn conflicting_markers_are_named_in_the_rejection() -> Check {
    // Annex C.2: a composite bears exactly one kind marker; a conflict names both
    // rather than letting the first-checked marker win.
    let rendered = rejection("{ $set: text, $view: text }")?;
    assert!(
        rendered.contains("`$set`") && rendered.contains("`$view`"),
        "the rejection must name both markers, got {rendered:?}"
    );
    Ok(())
}

#[test]
fn a_marker_beside_a_plain_field_is_rejected() -> Check {
    // An object type is either a struct or one marked composite, never both.
    let rendered = rejection("{ $set: text, name: text }")?;
    assert!(rendered.contains("name"), "got {rendered:?}");
    Ok(())
}

#[test]
fn unknown_marker_is_rejected() -> Check {
    let rendered = rejection("{ $bucket: text }")?;
    assert!(rendered.contains("$bucket"), "got {rendered:?}");
    Ok(())
}

#[test]
fn unclosed_object_rejected_with_location() -> Check {
    let source = "{ $key: text, $value: json";
    let diags = parse_err(source)?;
    let diag = diags.iter().next().ok_or("expected one diagnostic")?;
    assert!(diag.is_error());
    // The caret lands where the `}` should be: at the end of the input.
    assert_eq!(usize::try_from(diag.primary().span().bytes().start()), Ok(source.len()));
    Ok(())
}

#[test]
fn dangling_question_rejected() -> Check {
    // A `?` needs a base type before it.
    let diags = parse_err("?text")?;
    assert!(diags.has_errors());
    Ok(())
}

// A composite type nests through `{` / `}` (and the removed spellings through
// `<` / `>`), so it drives `pest`'s recursive descent — and the model's recursive
// type lowering — exactly as a bracketed expression drives the expression
// grammar. The pre-parse depth scan therefore counts both for type source;
// without it, a deep nest SIGABRTed the parser. These pin that guard (SPEC.md
// Annex A.2 / AGENTS.md "code must never panic").

/// A tower of `depth` nested `{ $set: … }` around `text`.
fn set_tower(depth: usize) -> String {
    format!("{}text{}", "{ $set: ".repeat(depth), " }".repeat(depth))
}

#[test]
fn deeply_nested_object_rejected_past_the_cap() -> Check {
    let diags = parse_err(&set_tower(40))?;
    let diag = diags.iter().next().ok_or("expected one diagnostic")?;
    assert!(
        diag.message().contains("nests") && diag.message().contains("32"),
        "expected a nesting-depth rejection naming the cap, got {:?}",
        diag.message()
    );
    Ok(())
}

#[test]
fn deeply_nested_type_at_pathological_depth_rejects_without_crashing() -> Check {
    // 50 000 nests: pre-fix this SIGABRTed in `pest`'s recursive descent. The
    // scan must reject it before a single grammar rule fires — for the object
    // form and for the removed parametric one alike, since both still reach the
    // parser. Reaching this assertion proves no overflow occurred.
    assert!(parse_err(&set_tower(50_000))?.has_errors());
    let legacy = format!("{}text{}", "optional<".repeat(50_000), ">".repeat(50_000));
    assert!(parse_err(&legacy)?.has_errors());
    Ok(())
}

#[test]
fn nesting_just_under_the_cap_is_accepted() -> Check {
    // 31 nests is below the cap, so the scan passes it and the grammar parses the
    // full tower down to the innermost `text`.
    let mut ty = parse_ok(&set_tower(31))?;
    let mut wraps = 0;
    while let TypeExprKind::Set(inner) = ty.kind {
        wraps += 1;
        ty = *inner;
    }
    assert_eq!(wraps, 31, "expected 31 nested sets");
    assert_eq!(ty.kind, TypeExprKind::Name("text".to_owned()));
    Ok(())
}
