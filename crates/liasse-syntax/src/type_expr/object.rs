//! Classifying an A.2 object type by the kind marker it bears, and rejecting the
//! removed parametric spellings.
//!
//! Annex A.2 gives Liasse one type syntax: every composite type is an object
//! carrying exactly one kind marker (`{ $set: T }`, `{ $key: K, $value: V }`,
//! `{ $view: T }`, `{ $ref: target }`), and a marker-free object is a struct.
//! The grammar therefore parses ONE object production; this module decides which
//! composite it names — mirroring, at the type level, the model layer's Annex-C.2
//! node-kind dispatch, so an ill-formed marker set is rejected once, by name,
//! with the same vocabulary a declaration would use.

use liasse_diag::{ByteSpan, Diagnostic, Span};

use super::ast::{SpannedType, TypeExprKind, TypeField};
use super::{Rule, TypeBuilder};
use pest::iterators::Pair;

/// One member of an object type: a `$marker: payload` or a plain `field: T`.
enum ObjectMember {
    /// A `$name: T` marker, or `$ref: target` (whose payload is a path).
    Marker {
        name: String,
        span: ByteSpan,
        payload: MarkerPayload,
    },
    /// A plain `field: T` / `field?: T` struct member.
    Field(TypeField),
}

enum MarkerPayload {
    Type(SpannedType),
    Path(String),
}

impl TypeBuilder {
    /// Lower one `{ ... }` object into the A.2 type it names.
    pub(super) fn object_type(&mut self, pair: Pair<'_, Rule>) -> Option<TypeExprKind> {
        let span = self.span(&pair);
        let mut markers: Vec<(String, ByteSpan, MarkerPayload)> = Vec::new();
        let mut fields: Vec<TypeField> = Vec::new();
        for member in pair.into_inner() {
            match self.object_member(member)? {
                ObjectMember::Marker {
                    name,
                    span,
                    payload,
                } => markers.push((name, span, payload)),
                ObjectMember::Field(field) => fields.push(field),
            }
        }
        if markers.is_empty() {
            return Some(TypeExprKind::Struct(fields));
        }
        if let (Some(field), Some((marker, ..))) = (fields.first(), markers.first()) {
            return self.reject(
                field.span,
                format!(
                    "`{}` is a plain field beside the kind marker `{marker}`: an object type is \
                     either a struct or one marked composite, never both",
                    field.name
                ),
                "move the field into its own struct type, or drop the marker",
            );
        }
        self.marked_type(span, markers)
    }

    /// The A.2 composite named by a non-empty marker set.
    fn marked_type(
        &mut self,
        span: ByteSpan,
        markers: Vec<(String, ByteSpan, MarkerPayload)>,
    ) -> Option<TypeExprKind> {
        let names: Vec<&str> = markers.iter().map(|(name, ..)| name.as_str()).collect();
        match names.as_slice() {
            ["$set"] => Some(TypeExprKind::Set(Box::new(self.payload_type(markers)?))),
            ["$view"] => Some(TypeExprKind::View(Box::new(self.payload_type(markers)?))),
            ["$ref"] => match markers.into_iter().next() {
                Some((_, _, MarkerPayload::Path(target))) => Some(TypeExprKind::Ref { target }),
                Some((_, span, MarkerPayload::Type(_))) => self.reject(
                    span,
                    "`$ref` names a target collection path, not a type",
                    "e.g. `{ $ref: /accounts }`",
                ),
                None => self.internal(span),
            },
            ["$key", "$value"] | ["$value", "$key"] => {
                let mut key = None;
                let mut value = None;
                for (name, span, payload) in markers {
                    let MarkerPayload::Type(ty) = payload else {
                        return self.reject(span, format!("`{name}` names a type"), "e.g. `text`");
                    };
                    if name == "$key" {
                        key = Some(ty);
                    } else {
                        value = Some(ty);
                    }
                }
                Some(TypeExprKind::Map(Box::new(key?), Box::new(value?)))
            }
            ["$key"] => self.reject(
                span,
                "`$key` alone does not name a type: a map type declares both its key and its \
                 value (§5.4)",
                "write `{ $key: K, $value: V }`",
            ),
            ["$value"] => self.reject(
                span,
                "`$value` needs a `$key`: it is what puts a declaration in map form, and a map's \
                 entries are keyed (§5.4)",
                "write `{ $key: K, $value: V }`",
            ),
            ["$optional"] => self.reject(
                span,
                "there is no `$optional` marker (A.2)",
                "optionality is the `?` suffix: `T?` at a type location, `field?: T` in an object",
            ),
            [one] => self.reject(
                span,
                format!("`{one}` does not name a type (A.2)"),
                "the type markers are `$set`, `$view`, `$ref`, and `$key` with `$value`",
            ),
            many => self.reject(
                span,
                format!(
                    "conflicting type markers {} on one object: a composite type bears exactly \
                     one kind marker (A.2, Annex C.2)",
                    many.iter()
                        .map(|m| format!("`{m}`"))
                        .collect::<Vec<_>>()
                        .join(" and ")
                ),
                "keep exactly one marker; only `$key` and `$value` combine, as a map",
            ),
        }
    }

    /// The single type payload of a one-marker object.
    fn payload_type(
        &mut self,
        markers: Vec<(String, ByteSpan, MarkerPayload)>,
    ) -> Option<SpannedType> {
        match markers.into_iter().next() {
            Some((_, _, MarkerPayload::Type(ty))) => Some(ty),
            Some((name, span, MarkerPayload::Path(_))) => {
                self.reject(span, format!("`{name}` names a type"), "e.g. `text`")
            }
            None => None,
        }
    }

    fn object_member(&mut self, pair: Pair<'_, Rule>) -> Option<ObjectMember> {
        let span = self.span(&pair);
        let inner = self.first_inner(&pair)?;
        match inner.as_rule() {
            Rule::ref_marker => {
                let target = inner.into_inner().nth(1)?;
                Some(ObjectMember::Marker {
                    name: "$ref".to_owned(),
                    span,
                    payload: MarkerPayload::Path(target.as_str().to_owned()),
                })
            }
            Rule::typed_marker => {
                let mut parts = inner.into_inner();
                let name = parts.next()?.as_str().to_owned();
                let ty = self.type_expr(parts.next()?)?;
                Some(ObjectMember::Marker {
                    name,
                    span,
                    payload: MarkerPayload::Type(ty),
                })
            }
            Rule::struct_field => Some(ObjectMember::Field(self.struct_field(inner)?)),
            _ => self.internal(span),
        }
    }

    /// Reject a removed parametric spelling by name, pointing at its A.2 object
    /// form. Never falls back to a type: the old syntax has no meaning left.
    pub(super) fn reject_legacy(&mut self, pair: &Pair<'_, Rule>) -> Option<TypeExprKind> {
        let span = self.span(pair);
        let ctor = pair
            .clone()
            .into_inner()
            .next()
            .map_or_else(String::new, |c| c.as_str().to_owned());
        let replacement = match ctor.as_str() {
            "optional" => "the `?` suffix — `T?`, or `field?: T` inside an object",
            "set" => "`{ $set: T }`",
            "view" => "`{ $view: T }`",
            "map" => "`{ $key: K, $value: V }`",
            "ref" => "`{ $ref: target }`",
            _ => "the object form of the type (A.2)",
        };
        self.reject(
            span,
            format!(
                "`{ctor}<…>` is not a type expression: Liasse has no parametric `<>` type form \
                 (A.2)"
            ),
            format!("write {replacement}"),
        )
    }

    fn reject<T>(
        &mut self,
        span: ByteSpan,
        message: impl Into<String>,
        help: impl Into<String>,
    ) -> Option<T> {
        self.diags.push(
            Diagnostic::error(message.into())
                .code("syntax-type")
                .primary(Span::new(self.source, span), "in this type expression")
                .help(help.into())
                .build(),
        );
        None
    }
}
