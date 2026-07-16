//! Mapping a parsed A.2 type expression to a canonical [`Type`] (SPEC.md Annex
//! A.2).
//!
//! Parsing the type-expression *syntax* is `liasse-syntax`'s job (it owns every
//! Liasse grammar and builds them on `pest`); this module owns only the
//! *semantics*: turning a spanned [`SpannedType`] tree into a canonical
//! [`liasse_value::Type`], resolving bare identifiers against the reusable shapes
//! declared in `$types` (§5.8). A produced [`Type`] is proof the spelling was a
//! well-formed A.2 type expression whose names all resolve.
//!
//! Scope note (CORE pass): the string form `ref<target>` is deferred to the
//! object form `{ "$ref": target }` (§5.6), which the state builder resolves
//! against the model tree; the A.2 `collection.$key` key-path form is a
//! documented seam for a later pass. Named references resolve only to
//! *scalar-shaped* reusable types (enums and static structs); a named
//! collection shape is resolved at the node layer, not here.

use std::collections::BTreeMap;

use liasse_diag::{Diagnostics, SourceMap};
use liasse_expr::ExprType;
use liasse_syntax::{parse_type_expression, SpannedType, TypeExprKind, TypeField};
use liasse_value::{StructType, Type};

/// A resolved-in-scope table of reusable scalar-shaped types (`$types`).
pub(crate) type NamedTypes = BTreeMap<String, Type>;

/// Parses one A.2 type expression and resolves it to a canonical [`Type`].
///
/// This is the model-layer boundary over `liasse-syntax`'s pest type-expression
/// parser: it registers `text` under a throwaway source (a type spelling has no
/// standalone location — its rejection is anchored at the declaring member by the
/// caller), parses it to a [`SpannedType`], then maps that tree to a [`Type`].
pub(crate) struct TypeParser;

impl TypeParser {
    /// Parse `text` as a complete type expression, or explain the rejection.
    pub(crate) fn parse(text: &str, named: &NamedTypes) -> Result<Type, String> {
        let mut sources = SourceMap::new();
        let id = sources.add_label("type", text.to_owned());
        let spanned =
            parse_type_expression(id, text).map_err(|diags| syntax_reason(&diags, text))?;
        map_type(&spanned, named)
    }
}

/// Parses a §8.3 mutation-prototype object (`{ name: type, opt?: type }`) into
/// its parameter contract. The prototype object is exactly an A.2 struct type,
/// so it goes through the same pest grammar as every other type expression;
/// only its interpretation (a parameter table rather than one struct value
/// type) differs.
pub(crate) fn parse_prototype(text: &str) -> Result<BTreeMap<String, ExprType>, String> {
    let mut sources = SourceMap::new();
    let id = sources.add_label("prototype", text.to_owned());
    let spanned = parse_type_expression(id, text).map_err(|diags| syntax_reason(&diags, text))?;
    let TypeExprKind::Struct(fields) = &spanned.kind else {
        return Err(format!(
            "a prototype declares its parameters as an object, e.g. `name({{ value: text }})` (§8.3); found `{text}`"
        ));
    };
    let named = NamedTypes::new();
    let mut params = BTreeMap::new();
    for field in fields {
        let inner = map_type(&field.ty, &named)?;
        let ty = if field.optional {
            Type::Optional(Box::new(inner))
        } else {
            inner
        };
        params.insert(field.name.clone(), ExprType::scalar(ty));
    }
    Ok(params)
}

/// The concise, single-line reason for a syntactic rejection, for callers that
/// anchor it at the declaring member's span rather than within the type text.
fn syntax_reason(diags: &Diagnostics, text: &str) -> String {
    let Some(diag) = diags.iter().next() else {
        return format!("`{text}` is not a well-formed type expression");
    };
    let mut reason = format!("`{text}` is not a valid type expression: {}", diag.message());
    if let Some(help) = diag.helps().first() {
        reason.push_str(" (");
        reason.push_str(help);
        reason.push(')');
    }
    reason
}

/// Map a spanned A.2 type tree to a canonical [`Type`], resolving `$types` names.
fn map_type(node: &SpannedType, named: &NamedTypes) -> Result<Type, String> {
    match &node.kind {
        TypeExprKind::Name(word) => map_name(word, named),
        // A postfix `T?` (A.2). `optional<T>?` would nest optionals, which the
        // type system does not represent — reject the redundant spelling.
        TypeExprKind::OptionalSuffix(inner) => {
            let inner = map_type(inner, named)?;
            if matches!(inner, Type::Optional(_)) {
                return Err("`optional<T>?` doubly declares an optional".to_owned());
            }
            Ok(Type::Optional(Box::new(inner)))
        }
        TypeExprKind::Optional(inner) => Ok(Type::Optional(Box::new(map_type(inner, named)?))),
        TypeExprKind::Set(inner) => Ok(Type::Set(Box::new(map_type(inner, named)?))),
        TypeExprKind::View(inner) => Ok(Type::View(Box::new(map_type(inner, named)?))),
        TypeExprKind::Map(key, value) => Ok(Type::Map(
            Box::new(map_type(key, named)?),
            Box::new(map_type(value, named)?),
        )),
        TypeExprKind::Ref { .. } => Err(ref_reason()),
        TypeExprKind::KeyPath(path) => Err(format!(
            "the `{path}` key-path type form (A.2) is resolved against the model tree in a later pass; declare the field's own type here"
        )),
        TypeExprKind::Struct(fields) => map_struct(fields, named),
    }
}

/// Resolve a bare (possibly dotted) name: a primitive keyword, a generic keyword
/// written without its argument, or a `$types` reference.
fn map_name(word: &str, named: &NamedTypes) -> Result<Type, String> {
    match word {
        "text" => Ok(Type::Text),
        "bool" => Ok(Type::Bool),
        "int" => Ok(Type::Int),
        "decimal" => Ok(Type::Decimal),
        "bytes" => Ok(Type::Bytes),
        "uuid" => Ok(Type::Uuid),
        "date" => Ok(Type::Date),
        "timestamp" => Ok(Type::timestamp()),
        "duration" => Ok(Type::Duration),
        "period" => Ok(Type::Period),
        "json" => Ok(Type::Json),
        "blob" => Ok(Type::Blob),
        // A generic keyword spelled without its `<...>` argument.
        "optional" | "set" | "view" => Err(format!("`{word}` requires a `<T>` argument")),
        "map" => Err("`map` requires a `<K, V>` argument".to_owned()),
        "ref" => Err(ref_reason()),
        other => named
            .get(other)
            .cloned()
            .ok_or_else(|| format!("`{other}` is not a known type or a declared `$types` name")),
    }
}

/// Map a `{ field: T, optional_field?: U }` struct type (§5.3, A.2). A field
/// marked optional with a `?` after its name wraps its type in `optional<T>`.
fn map_struct(fields: &[TypeField], named: &NamedTypes) -> Result<Type, String> {
    let mut mapped: Vec<(String, Type)> = Vec::with_capacity(fields.len());
    for field in fields {
        let inner = map_type(&field.ty, named)?;
        let ty = if field.optional {
            Type::Optional(Box::new(inner))
        } else {
            inner
        };
        mapped.push((field.name.clone(), ty));
    }
    Ok(Type::Struct(StructType::new(mapped)))
}

fn ref_reason() -> String {
    "declare a reference with the object form `{ \"$ref\": target }` (§5.6) rather than the `ref<...>` string form".to_owned()
}
