//! Document-form parsing: the Hjson authoring surface and the strict-JSON
//! built form must produce the same spanned tree, and malformed input must
//! surface a located, actionable diagnostic.
//!
//! Tests return `Result` and route failures through `?` so the crate's
//! deny-by-default lints (no `unwrap`/`expect`/`panic!`/indexing) hold here too.

use liasse_diag::{Diagnostics, SourceId, SourceMap};
use liasse_syntax::{parse_document, DocMember, DocValueKind, SpannedDocument};

type Check = Result<(), String>;

fn parse(text: &str) -> Result<(SpannedDocument, SourceMap, SourceId), String> {
    let mut sources = SourceMap::new();
    let id = sources.add_label("doc", text);
    let doc = parse_document(id, text).map_err(|d| d.render(&sources))?;
    Ok((doc, sources, id))
}

fn parse_err(text: &str) -> Result<Diagnostics, String> {
    let mut sources = SourceMap::new();
    let id = sources.add_label("doc", text);
    match parse_document(id, text) {
        Ok(_) => Err(format!("{text:?} parsed but a rejection was expected")),
        Err(diags) => Ok(diags),
    }
}

fn members(doc: &SpannedDocument) -> Result<&[DocMember], String> {
    match &doc.root().kind {
        DocValueKind::Object(m) => Ok(m),
        other => Err(format!("expected an object root, got {other:?}")),
    }
}

fn member_value<'d>(doc: &'d SpannedDocument, name: &str) -> Result<&'d DocValueKind, String> {
    members(doc)?
        .iter()
        .find(|m| m.name.text == name)
        .map(|m| &m.value.kind)
        .ok_or_else(|| format!("member {name:?} not found"))
}

#[test]
fn parses_authoring_scalars_and_nesting() -> Check {
    let src = r#"{
  "$liasse": 1
  "$app": "example.tasks@1.0.0"
  "$model": { "done": false, "note": null }
}"#;
    let (doc, ..) = parse(src)?;
    // Numbers keep their raw text so decimal scale/int spelling survive.
    assert_eq!(
        member_value(&doc, "$liasse")?,
        &DocValueKind::Number("1".to_owned())
    );
    assert_eq!(
        member_value(&doc, "$app")?,
        &DocValueKind::String("example.tasks@1.0.0".to_owned())
    );
    let DocValueKind::Object(inner) = member_value(&doc, "$model")? else {
        return Err("expected nested object".to_owned());
    };
    assert_eq!(inner.len(), 2);
    assert_eq!(
        inner.first().map(|m| &m.value.kind),
        Some(&DocValueKind::Bool(false))
    );
    assert_eq!(
        inner.get(1).map(|m| &m.value.kind),
        Some(&DocValueKind::Null)
    );
    Ok(())
}

#[test]
fn hjson_conveniences_match_strict_json() -> Check {
    // Comments, unquoted names, omitted commas, and a trailing comma.
    let hjson = r#"{
  // the language generation
  liasse: 1
  app: "t.case@1.0.0"
  nums: [1, 2 3]
  flag: true,
}"#;
    let strict = r#"{"liasse":1,"app":"t.case@1.0.0","nums":[1,2,3],"flag":true}"#;
    let (a, ..) = parse(hjson)?;
    let (b, ..) = parse(strict)?;
    // The authoring conveniences carry no meaning after the build: the two
    // trees are structurally identical once spans (which necessarily differ)
    // are projected away.
    assert_eq!(shape(&a.root().kind), shape(&b.root().kind));
    assert!(shape(&a.root().kind).contains("nums=[1,2,3]"));
    Ok(())
}

/// A span-free textual projection of a value, so two documents that differ
/// only in layout compare equal.
fn shape(kind: &DocValueKind) -> String {
    match kind {
        DocValueKind::Null => "null".to_owned(),
        DocValueKind::Bool(b) => b.to_string(),
        DocValueKind::Number(n) => n.clone(),
        DocValueKind::String(s) => format!("{s:?}"),
        DocValueKind::Array(items) => {
            let inner: Vec<String> = items.iter().map(|v| shape(&v.kind)).collect();
            format!("[{}]", inner.join(","))
        }
        DocValueKind::Object(entries) => {
            let inner: Vec<String> = entries
                .iter()
                .map(|m| format!("{}={}", m.name.text, shape(&m.value.kind)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

#[test]
fn triple_quote_string_is_deindented() -> Check {
    // The opening ''' sits at column 8; each body line loses that gutter, and
    // the first newline after ''' is dropped (Hjson rule).
    let src = "{\n  body: '''\n        line one\n        line two'''\n}";
    let (doc, ..) = parse(src)?;
    assert_eq!(
        member_value(&doc, "body")?,
        &DocValueKind::String("line one\nline two".to_owned())
    );
    Ok(())
}

#[test]
fn value_span_locates_the_exact_bytes() -> Check {
    let src = r#"{ "app": "vendor.app@1.0.0" }"#;
    let (doc, sources, id) = parse(src)?;
    let member = members(&doc)?.first().ok_or("expected one member")?;
    // The value span must slice back to the quoted token including its quotes.
    let source = sources.get(id).ok_or("source not registered")?;
    let sliced = source.slice(member.value.span).ok_or("span out of range")?;
    assert_eq!(sliced, "\"vendor.app@1.0.0\"");
    Ok(())
}

#[test]
fn missing_closing_brace_points_at_end_with_fix() -> Check {
    let diags = parse_err("{ \"a\": 1")?;
    assert!(diags.has_errors());
    let diag = diags.iter().next().ok_or("expected one diagnostic")?;
    // A fix hint names the missing delimiter.
    assert!(
        diag.helps().iter().any(|h| h.contains("closing `}`")),
        "expected a closing-brace hint, got {:?}",
        diag.helps()
    );
    // A secondary label points back at the opener at byte 0.
    let opener = diag
        .secondaries()
        .iter()
        .find(|l| l.message().contains("unclosed"))
        .ok_or("expected an opener label")?;
    assert_eq!(opener.span().bytes().start(), 0);
    Ok(())
}

#[test]
fn unterminated_string_is_diagnosed() -> Check {
    let diags = parse_err("{ \"a\": \"oops }")?;
    let diag = diags.iter().next().ok_or("expected one diagnostic")?;
    assert!(
        diag.helps().iter().any(|h| h.contains("closing quote")),
        "expected an unterminated-string hint, got {:?}",
        diag.helps()
    );
    Ok(())
}

#[test]
fn quoteless_string_value_is_rejected() -> Check {
    // Scoped-out Hjson feature: a bare word as a value is not accepted (the
    // spec never uses it). It must fail rather than silently become a string.
    let diags = parse_err("{ name: bareword }")?;
    assert!(diags.has_errors());
    Ok(())
}

#[test]
fn deeply_nested_input_is_rejected_before_the_parser_recurses() -> Check {
    // 60k nested `[` overflows pest's recursive descent (SIGABRT). The pre-parse
    // nesting guard must reject it with a located diagnostic and, above all, not
    // crash the process.
    let deep = "[".repeat(60_000);
    let diags = parse_err(&deep)?;
    let diag = diags.iter().next().ok_or("expected one diagnostic")?;
    assert!(
        diag.message().contains("512"),
        "the message must state the 512 limit, got {:?}",
        diag.message()
    );
    // The caret points at the first bracket past the cap: the 513th `[`, byte 512.
    assert_eq!(diag.primary().span().bytes().start(), 512);
    assert!(
        diag.helps().iter().any(|h| h.contains("nest")),
        "expected a restructuring hint, got {:?}",
        diag.helps()
    );
    Ok(())
}

#[test]
fn nesting_just_under_the_cap_is_accepted() -> Check {
    // Depth 511 is below the 512 cap: the guard passes it and pest parses the
    // full 511-deep array of arrays down to the central `1`.
    let src = format!("{}1{}", "[".repeat(511), "]".repeat(511));
    let (doc, ..) = parse(&src)?;
    assert!(matches!(doc.root().kind, DocValueKind::Array(_)));
    Ok(())
}
