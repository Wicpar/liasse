//! Keyed-collection construction: `$key` and `$unique` validation (SPEC.md
//! §5.4, §5.7, A.8). Split from the field-value forms in [`super::fields`]; the
//! shape-walking core lives in [`super`]. Continues the same [`Builder`] impl.

use liasse_syntax::{DocMember, DocValue};
use liasse_value::{RefTarget, StructType, Type};

use crate::doc::DocValueExt;
use crate::names::DeclName;
use crate::report::{code, Reporter};
use crate::state::{Collection, Node, Shape};

use super::Builder;

impl<'a> Builder<'a> {
    pub(super) fn collection(
        &mut self,
        reporter: &mut Reporter,
        value: &'a DocValue,
        path: &[String],
    ) -> Collection {
        let members = value.as_object().unwrap_or(&[]);
        let shape = self.build_shape(reporter, members, path, false);
        let key_member = value.member("$key");
        let (key, key_span) = self.key_fields(reporter, key_member, &shape);
        let mut unique = value
            .member("$unique")
            .map(|m| self.unique_keys(reporter, &m.value, &shape))
            .unwrap_or_default();
        // §5.7: `$unique: true` on a field adds one single-field candidate key
        // for that field, equivalent to the array spelling `$unique: [field]`;
        // its component must be key-eligible (A.8), enforced here so the
        // shorthand is not silently weaker than the array form.
        unique.extend(self.field_unique_keys(reporter, &shape));
        let consumes = value.member("$consumes").is_some();
        Collection {
            key,
            key_span,
            unique,
            consumes,
            shape,
        }
    }

    /// Parse and validate `$key` (§5.4, A.8): names must be declared,
    /// key-eligible, non-optional fields.
    fn key_fields(
        &self,
        reporter: &mut Reporter,
        key_member: Option<&DocMember>,
        shape: &Shape,
    ) -> (Vec<DeclName>, liasse_diag::ByteSpan) {
        let Some(member) = key_member else {
            return (Vec::new(), liasse_diag::ByteSpan::point(0));
        };
        let span = member.value.span;
        let names = self.key_name_list(reporter, &member.value);
        let mut key = Vec::new();
        for (name, name_span) in names {
            match self.key_field_type(shape, &name) {
                Ok(name) => key.push(name),
                Err(reason) => reporter.reject_hint(
                    name_span,
                    code::KEY,
                    reason,
                    "a `$key` must name a declared, required, key-eligible field (§5.4, A.8)",
                ),
            }
        }
        (key, span)
    }

    /// The field names of a `$key`: a string names one, an array names several.
    fn key_name_list(
        &self,
        reporter: &mut Reporter,
        value: &DocValue,
    ) -> Vec<(String, liasse_diag::ByteSpan)> {
        if let Some(text) = value.as_string() {
            return vec![(text.to_owned(), value.span)];
        }
        if let Some(items) = value.as_array() {
            return items
                .iter()
                .filter_map(|item| item.as_string().map(|t| (t.to_owned(), item.span)))
                .collect();
        }
        reporter.reject_hint(
            value.span,
            code::KEY,
            "`$key` names one field or an array of fields",
            "e.g. `\"$key\": \"id\"` or `\"$key\": [\"country\", \"code\"]`",
        );
        Vec::new()
    }

    /// Validate that `name` is a declared, key-eligible, non-optional field.
    fn key_field_type(&self, shape: &Shape, name: &str) -> Result<DeclName, String> {
        let member = shape.member(name).ok_or_else(|| {
            format!("`$key` names `{name}`, which is not a declared field of the collection")
        })?;
        let ty = match &member.node {
            Node::Scalar(field) if field.is_writable() => &field.ty,
            // §A.9/§10.3: a `$ref` key field takes the exact key type of its
            // target collection or keyed view — which the target's own `$key`
            // already proved key-eligible — so a required ref is a valid key
            // (the idiomatic scoped-membership `$key: "account"` over a
            // `{ $ref: "/accounts" }` field). Only optionality excludes it.
            Node::Reference(reference) => {
                return if reference.optional {
                    Err(format!(
                        "key field `{name}` is an optional ref; optional types are excluded from row keys (A.8)"
                    ))
                } else {
                    DeclName::parse(name).map_err(|_| format!("`{name}` is not a valid field name"))
                };
            }
            // A.8: a `$key` MAY name a "struct composed solely of key-eligible
            // required fields". An inline struct field builds to a `Node::Struct`
            // (`build/shapes.rs`); build its struct `Type` from the shape and let
            // `Type::is_key_eligible` judge it — accepting an all-eligible struct
            // and rejecting one that launders an ineligible member (json,
            // optional, …) with a diagnostic naming that member.
            Node::Struct(shape) => {
                return Self::struct_key_type(shape).and_then(|_key_type| {
                    DeclName::parse(name).map_err(|_| format!("`{name}` is not a valid field name"))
                });
            }
            _ => {
                return Err(format!(
                    "`$key` field `{name}` must be a writable scalar, a struct of key-eligible required fields, or a required ref field"
                ));
            }
        };
        if matches!(ty, Type::Optional(_)) {
            return Err(format!(
                "key field `{name}` is optional; optional types are excluded from row keys (A.8)"
            ));
        }
        if !ty.is_key_eligible() {
            return Err(format!(
                "key field `{name}` has type `{}`, which is not key-eligible (A.8)",
                ty.name()
            ));
        }
        DeclName::parse(name).map_err(|_| format!("`{name}` is not a valid field name"))
    }

    /// The key `Type` of a struct field named as a `$key` component (A.8:
    /// "structs composed solely of key-eligible required fields"). Builds the
    /// struct `Type` from the shape's members so [`Type::is_key_eligible`] judges
    /// the whole; on the first ineligible member it returns a diagnostic naming
    /// that member and its type, so a struct key laundering an ineligible member
    /// (a `json`/`optional`/… field one level down) is rejected for the member,
    /// not merely "not a scalar".
    fn struct_key_type(shape: &Shape) -> Result<Type, String> {
        let mut fields = Vec::with_capacity(shape.members.len());
        for member in &shape.members {
            let name = member.name.as_str();
            let ty = Self::member_key_type(name, &member.node)?;
            if !ty.is_key_eligible() {
                return Err(format!(
                    "struct key component '{name}' is {}, which is not key-eligible (A.8)",
                    ty.name()
                ));
            }
            fields.push((name.to_owned(), ty));
        }
        Ok(Type::Struct(StructType::new(fields)))
    }

    /// The declared `Type` of one struct member, for the A.8 key-eligibility
    /// judgement. A nested struct recurses (so a deep ineligible member is named
    /// at its own level); a ref/set contributes its non-key-eligible type so the
    /// containing struct is refused. A member with no value type usable as a key
    /// (a computed view, a nested collection, a `$like` shape) is refused
    /// directly, naming the member.
    fn member_key_type(name: &str, node: &Node) -> Result<Type, String> {
        match node {
            Node::Scalar(field) => Ok(field.ty.clone()),
            Node::Struct(inner) => Self::struct_key_type(inner),
            Node::Reference(reference) => Ok(Type::Ref(RefTarget::for_key(&reference.key_type))),
            Node::Set(set) => Ok(Type::Set(Box::new(set.element.clone()))),
            Node::View(_) | Node::Collection(_) | Node::Named(_) => Err(format!(
                "struct key component '{name}' is not a field with a key-eligible type (A.8)"
            )),
        }
    }

    /// Parse `$unique` candidate keys (§5.7).
    fn unique_keys(
        &self,
        reporter: &mut Reporter,
        value: &DocValue,
        shape: &Shape,
    ) -> Vec<Vec<DeclName>> {
        let Some(items) = value.as_array() else {
            reporter.reject_hint(
                value.span,
                code::KEY,
                "`$unique` is an array of candidate keys",
                "each entry is a field name or an array of field names",
            );
            return Vec::new();
        };
        let mut candidates = Vec::new();
        for item in items {
            let names = self.key_name_list(reporter, item);
            let mut candidate = Vec::new();
            for (name, name_span) in names {
                match self.unique_field(shape, &name) {
                    Ok(name) => candidate.push(name),
                    Err(reason) => reporter.reject(name_span, code::KEY, reason),
                }
            }
            if !candidate.is_empty() {
                candidates.push(candidate);
            }
        }
        candidates
    }

    /// The single-field candidate keys contributed by field-level `$unique: true`
    /// shorthands (§5.7), each validated for key-eligibility (A.8) exactly as an
    /// array-form `$unique` entry is.
    fn field_unique_keys(&self, reporter: &mut Reporter, shape: &Shape) -> Vec<Vec<DeclName>> {
        let mut candidates = Vec::new();
        for member in &shape.members {
            let Node::Scalar(field) = &member.node else { continue };
            if !field.unique {
                continue;
            }
            match self.unique_field(shape, member.name.as_str()) {
                Ok(name) => candidates.push(vec![name]),
                Err(reason) => reporter.reject(field.span, code::KEY, reason),
            }
        }
        candidates
    }

    /// A candidate-key component may be optional but its present type must be
    /// key-eligible (A.8).
    fn unique_field(&self, shape: &Shape, name: &str) -> Result<DeclName, String> {
        let member = shape
            .member(name)
            .ok_or_else(|| format!("`$unique` names `{name}`, not a declared field"))?;
        let ty = match &member.node {
            Node::Scalar(field) => &field.ty,
            // §5.7/A.8/A.9: a `$ref` candidate-key component contributes its
            // target collection's already-validated eligible key type (A.9),
            // exactly as a primary `$key` ref component does (`key_field_type`),
            // so it is a valid candidate key. A candidate key MAY be optional
            // (A.8: "the candidate-key fields themselves MAY be optional"), so —
            // unlike a primary row key — an *optional* ref is admissible too; the
            // row simply does not participate in the constraint while it is
            // `none`. This matches the optional-scalar handling below.
            Node::Reference(_) => {
                return DeclName::parse(name)
                    .map_err(|_| format!("`{name}` is not a valid field name"));
            }
            _ => return Err(format!("candidate-key field `{name}` must be a scalar or ref field")),
        };
        let base = match ty {
            Type::Optional(inner) => inner.as_ref(),
            other => other,
        };
        if !base.is_key_eligible() {
            return Err(format!(
                "candidate-key field `{name}` has non-key-eligible type `{}` (A.8)",
                base.name()
            ));
        }
        DeclName::parse(name).map_err(|_| format!("`{name}` is not a valid field name"))
    }
}
