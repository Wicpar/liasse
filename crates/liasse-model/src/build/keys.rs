//! Keyed-collection construction: `$key` and `$unique` validation (SPEC.md
//! §5.4, §5.7, A.8). Split from the field-value forms in [`super::fields`]; the
//! shape-walking core lives in [`super`]. Continues the same [`Builder`] impl.

use liasse_syntax::{DocMember, DocValue};
use liasse_value::Type;

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
        let unique = value
            .member("$unique")
            .map(|m| self.unique_keys(reporter, &m.value, &shape))
            .unwrap_or_default();
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
            _ => {
                return Err(format!(
                    "`$key` field `{name}` must be a writable scalar field"
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

    /// A candidate-key component may be optional but its present type must be
    /// key-eligible (A.8).
    fn unique_field(&self, shape: &Shape, name: &str) -> Result<DeclName, String> {
        let member = shape
            .member(name)
            .ok_or_else(|| format!("`$unique` names `{name}`, not a declared field"))?;
        let ty = match &member.node {
            Node::Scalar(field) => &field.ty,
            _ => return Err(format!("candidate-key field `{name}` must be a scalar field")),
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
