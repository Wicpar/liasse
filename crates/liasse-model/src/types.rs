//! The type-expression grammar (SPEC.md Annex A.2).
//!
//! A field's type is authored as a string — `"text"`, `"text?"`,
//! `"optional<map<text, json>>"`. [`TypeParser`] turns that string into a
//! canonical [`Type`], resolving bare identifiers against the reusable shapes
//! declared in `$types` (§5.8). A parsed [`Type`] is proof the spelling was a
//! well-formed A.2 type expression.
//!
//! Scope note (CORE pass): the string form `ref<target>` is deferred to the
//! object form `{ "$ref": target }` (§5.6), which the state builder resolves
//! against the model tree; a bare `collection.$key` type reference (A.2) is a
//! documented seam for a later pass. Named references resolve only to
//! *scalar-shaped* reusable types (enums and static structs); a named
//! collection shape is resolved at the node layer, not here.

use std::collections::BTreeMap;

use liasse_value::{StructType, Type};

/// A resolved-in-scope table of reusable scalar-shaped types (`$types`).
pub(crate) type NamedTypes = BTreeMap<String, Type>;

/// A recursive-descent parser over one A.2 type expression.
pub(crate) struct TypeParser<'a> {
    rest: &'a str,
    named: &'a NamedTypes,
}

impl<'a> TypeParser<'a> {
    /// Parse `text` as a complete type expression, or explain the rejection.
    pub(crate) fn parse(text: &str, named: &'a NamedTypes) -> Result<Type, String> {
        let mut parser = TypeParser {
            rest: text,
            named,
        };
        let ty = parser.type_expr()?;
        parser.skip_ws();
        if !parser.rest.is_empty() {
            return Err(format!(
                "unexpected trailing text `{}` in type expression `{text}`",
                parser.rest
            ));
        }
        Ok(ty)
    }

    fn skip_ws(&mut self) {
        self.rest = self.rest.trim_start();
    }

    /// Consume `token` if it appears next (after whitespace), reporting whether
    /// it did.
    fn eat(&mut self, token: char) -> bool {
        self.skip_ws();
        if let Some(remainder) = self.rest.strip_prefix(token) {
            self.rest = remainder;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, token: char) -> Result<(), String> {
        if self.eat(token) {
            Ok(())
        } else {
            Err(format!("expected `{token}` in type expression"))
        }
    }

    /// A postfix `?` turns any base type into `optional<T>` (A.2).
    fn type_expr(&mut self) -> Result<Type, String> {
        let base = self.base()?;
        if self.eat('?') {
            if matches!(base, Type::Optional(_)) {
                return Err("`optional<T>?` doubly declares an optional".to_owned());
            }
            Ok(Type::Optional(Box::new(base)))
        } else {
            Ok(base)
        }
    }

    fn base(&mut self) -> Result<Type, String> {
        self.skip_ws();
        if self.rest.starts_with('{') {
            return self.struct_type();
        }
        let word = self.ident()?;
        match word.as_str() {
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
            "optional" => Ok(Type::Optional(Box::new(self.one_arg("optional")?))),
            "set" => Ok(Type::Set(Box::new(self.one_arg("set")?))),
            "view" => Ok(Type::View(Box::new(self.one_arg("view")?))),
            "map" => {
                self.expect('<')?;
                let key = self.type_expr()?;
                self.expect(',')?;
                let value = self.type_expr()?;
                self.expect('>')?;
                Ok(Type::Map(Box::new(key), Box::new(value)))
            }
            "ref" => Err(
                "declare a reference with the object form `{ \"$ref\": target }` (§5.6) rather than the `ref<...>` string form".to_owned(),
            ),
            other => self.named.get(other).cloned().ok_or_else(|| {
                format!("`{other}` is not a known type or a declared `$types` name")
            }),
        }
    }

    fn one_arg(&mut self, name: &str) -> Result<Type, String> {
        self.expect('<')
            .map_err(|_| format!("`{name}` requires a `<T>` argument"))?;
        let inner = self.type_expr()?;
        self.expect('>')?;
        Ok(inner)
    }

    fn struct_type(&mut self) -> Result<Type, String> {
        self.expect('{')?;
        let mut fields: Vec<(String, Type)> = Vec::new();
        if !self.eat('}') {
            loop {
                let name = self.ident()?;
                let optional = self.eat('?');
                self.expect(':')?;
                let inner = self.type_expr()?;
                let field_ty = if optional {
                    Type::Optional(Box::new(inner))
                } else {
                    inner
                };
                fields.push((name, field_ty));
                if self.eat('}') {
                    break;
                }
                self.expect(',')?;
                if self.eat('}') {
                    break;
                }
            }
        }
        Ok(Type::Struct(StructType::new(fields)))
    }

    fn ident(&mut self) -> Result<String, String> {
        self.skip_ws();
        let end = self
            .rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
            .unwrap_or(self.rest.len());
        if end == 0 {
            return Err(format!(
                "expected a type name in type expression, found `{}`",
                self.rest
            ));
        }
        let (word, rest) = self.rest.split_at(end);
        self.rest = rest;
        Ok(word.to_owned())
    }
}
