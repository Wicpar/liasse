//! Field-value node construction (SPEC.md Annex C.3): turning one member's
//! value into a [`Node`] — scalar/computed fields, enums, refs, sets, static
//! structs, expanded fields, `$like`, and the `$check` forms. Split from the
//! shape-walking core in [`super`], which owns names, keys, and mutations.

use liasse_syntax::{DocMember, DocValue};
use liasse_value::{EnumType, Type};

use crate::doc::DocValueExt;
use crate::names::DeclName;
use crate::report::{code, Reporter};
use crate::state::{Check, Collection, ExprSource, Node, Reference, ScalarField, SetField, Shape};
use crate::types::TypeParser;

use super::{absolute_path, default_source, expr_source, placeholder, Builder};

impl<'a> Builder<'a> {
    /// Parse a string field value: `"= expr"` (computed), `"T = default"`,
    /// `"T"`/`"T?"`, or a bare `$types` name.
    pub(super) fn scalar_from_string(&self, reporter: &mut Reporter, value: &DocValue, span: liasse_diag::ByteSpan) -> Node {
        let Some(text) = value.as_string() else {
            return Node::Scalar(placeholder(span));
        };
        let trimmed = text.trim_start();
        if let Some(rest) = trimmed.strip_prefix('=') {
            return Node::Scalar(ScalarField {
                ty: Type::Json,
                computed: Some(ExprSource {
                    text: rest.trim().to_owned(),
                    span,
                }),
                default: None,
                normalize: None,
                checks: Vec::new(),
                unique: false,
                span,
            });
        }
        let (type_str, default) = match text.split_once('=') {
            Some((lhs, rhs)) => (
                lhs.trim(),
                Some(ExprSource {
                    text: rhs.trim().to_owned(),
                    span,
                }),
            ),
            None => (text.trim(), None),
        };
        match TypeParser::parse(type_str, &self.named) {
            Ok(ty) => Node::Scalar(ScalarField {
                ty,
                computed: None,
                default,
                normalize: None,
                checks: Vec::new(),
                unique: false,
                span,
            }),
            Err(reason) => {
                if default.is_none() && self.type_names.contains(type_str) {
                    Node::Named(type_str.to_owned())
                } else {
                    reporter.reject(span, code::TYPE, reason);
                    Node::Scalar(placeholder(span))
                }
            }
        }
    }

    /// Dispatch an object-valued member on its shape marker (Annex C.2).
    pub(super) fn object_node(&mut self, reporter: &mut Reporter, member: &'a DocMember, path: &[String]) -> Node {
        let value = &member.value;
        if value.member("$keyring").is_some() {
            return self.keyring_node(reporter, value);
        }
        if value.member("$modules").is_some() {
            return self.modules_node(reporter, value, path);
        }
        if value.member("$key").is_some() {
            return Node::Collection(Box::new(self.collection(reporter, value, path)));
        }
        if value.member("$bucket").is_some() {
            return self.source_bucket_node(value, path);
        }
        if let Some(set) = value.member("$set") {
            return self.set_node(reporter, value, set);
        }
        if let Some(view) = value.member("$view") {
            return self.view_node(reporter, view);
        }
        if value.member("$ref").is_some() {
            return self.ref_node(reporter, value);
        }
        if let Some(en) = value.member("$enum") {
            return self.enum_node(reporter, en);
        }
        if value.member("$like").is_some() {
            return self.like_node(reporter, value, path);
        }
        if value.member("$type").is_some() {
            return self.expanded_field(reporter, value);
        }
        // A plain object is a static struct (§5.3).
        match value.as_object() {
            Some(members) => Node::Struct(self.build_shape(reporter, members, path, false)),
            None => Node::Scalar(placeholder(value.span)),
        }
    }

    /// A `$bucket` object without `$key` is a source-backed, read-only bucket
    /// collection (§14.4/§14.6). Its output fields read the source row through
    /// `$source`, a structural binding the general tree checker does not carry,
    /// so the node is opaque here: the `$bucket` declaration is collected for the
    /// bucket phase (which types the interval expressions in the source scope),
    /// and the path is recorded read-only for the mutation phase. Typing the
    /// output fields (`plan: "= $source.plan"`) and the custom-`$key` form of
    /// §14.6 are documented seams.
    fn source_bucket_node(&mut self, value: &'a DocValue, path: &[String]) -> Node {
        self.source_buckets.push(absolute_path(path));
        if let Some(bucket) = value.member("$bucket") {
            self.buckets.push(super::RawDecl {
                path: path.to_vec(),
                span: bucket.span,
                value: &bucket.value,
            });
        }
        Node::Scalar(placeholder(value.span))
    }

    fn collection(&mut self, reporter: &mut Reporter, value: &'a DocValue, path: &[String]) -> Collection {
        let members = value.as_object().unwrap_or(&[]);
        let shape = self.build_shape(reporter, members, path, false);
        let key_member = value.member("$key");
        let (key, key_span) = self.key_fields(reporter, key_member, &shape);
        let unique = value
            .member("$unique")
            .map(|m| self.unique_keys(reporter, &m.value, &shape))
            .unwrap_or_default();
        Collection {
            key,
            key_span,
            unique,
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
    fn key_name_list(&self, reporter: &mut Reporter, value: &DocValue) -> Vec<(String, liasse_diag::ByteSpan)> {
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
    fn unique_keys(&self, reporter: &mut Reporter, value: &DocValue, shape: &Shape) -> Vec<Vec<DeclName>> {
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

    /// A `$keyring` managed-keyring declaration (§17.1, C.16). Its policy shape
    /// is validated inline (no cross-model scope is needed); provider capability
    /// resolution is a documented runtime seam. The node is opaque: a reference
    /// to the ring resolves through a host namespace, not ordinary field access.
    fn keyring_node(&self, reporter: &mut Reporter, value: &DocValue) -> Node {
        if let Some(keyring) = value.member("$keyring") {
            crate::keyring::check(reporter, &keyring.value);
        }
        for member in value.as_object().unwrap_or(&[]) {
            if member.name.text != "$keyring" {
                reporter.reject(
                    member.span,
                    code::RESERVED_MEMBER,
                    format!("`{}` may not accompany a `$keyring` declaration", member.name.text),
                );
            }
        }
        Node::Scalar(placeholder(value.span))
    }

    /// A `$modules` module space (§13.2, C.15). The composition grammar
    /// (`$expose`/`$interfaces`/`$auth`) is validated for shape; instance
    /// installation and cross-package resolution are runtime seams. The node is
    /// opaque for ordinary field typing.
    fn modules_node(&self, reporter: &mut Reporter, value: &DocValue, _path: &[String]) -> Node {
        if let Some(space) = value.member("$modules") {
            crate::module::check_space(reporter, &space.value);
        }
        for member in value.as_object().unwrap_or(&[]) {
            if member.name.text != "$modules" {
                reporter.reject(
                    member.span,
                    code::RESERVED_MEMBER,
                    format!("`{}` may not accompany a `$modules` space", member.name.text),
                );
            }
        }
        Node::Scalar(placeholder(value.span))
    }

    fn set_node(&mut self, reporter: &mut Reporter, value: &DocValue, set: &DocMember) -> Node {
        let element = self.shape_or_type(reporter, &set.value);
        // A set of refs declares `$set: { $ref: ... }`; other object element
        // shapes are a documented CORE seam (element must be a scalar type).
        Node::Set(SetField {
            element,
            span: value.span,
        })
    }

    /// Resolve a `$set` element type: a type string, or a ref element.
    fn shape_or_type(&mut self, reporter: &mut Reporter, value: &DocValue) -> Type {
        if let Some(text) = value.as_string() {
            return match TypeParser::parse(text.trim(), &self.named) {
                Ok(ty) => ty,
                Err(reason) => {
                    reporter.reject(value.span, code::TYPE, reason);
                    Type::Json
                }
            };
        }
        if value.member("$ref").is_some()
            && let Node::Reference(reference) = self.ref_node(reporter, value)
        {
            return Type::Ref(liasse_value::RefTarget::Scalar(Box::new(reference.key_type)));
        }
        reporter.reject_hint(
            value.span,
            code::TYPE,
            "a `$set` element must be a type or a `{ $ref: ... }`",
            "e.g. `\"tags\": { \"$set\": \"text\" }`",
        );
        Type::Json
    }

    fn view_node(&mut self, _reporter: &mut Reporter, view: &DocMember) -> Node {
        let expr = ExprSource {
            text: view.value.as_string().unwrap_or_default().to_owned(),
            span: view.value.span,
        };
        crate::state::Node::View(crate::state::ViewDecl {
            expr,
            row: liasse_expr::RowType::keyless(std::iter::empty::<(String, liasse_expr::ExprType)>()),
        })
    }

    fn ref_node(&mut self, reporter: &mut Reporter, value: &DocValue) -> Node {
        let target = value
            .member("$ref")
            .and_then(|m| m.value.as_string())
            .unwrap_or_default()
            .to_owned();
        let optional = value
            .member("$optional")
            .and_then(|m| m.value.as_bool())
            .unwrap_or(false);
        let on_delete = value.member("$on_delete").map(|m| ExprSource {
            text: m.value.as_string().unwrap_or_default().to_owned(),
            span: m.value.span,
        });
        if target.is_empty() {
            reporter.reject(value.span, code::REF, "`$ref` must name a target collection path");
        }
        // key_type is resolved against the tree in the ref-resolution pass.
        Node::Reference(Reference {
            target,
            key_type: Type::Json,
            optional,
            on_delete,
            span: value.span,
        })
    }

    fn enum_node(&mut self, reporter: &mut Reporter, en: &DocMember) -> Node {
        let Some(items) = en.value.as_array() else {
            reporter.reject(en.value.span, code::ENUM, "`$enum` must be an array of labels");
            return Node::Scalar(placeholder(en.value.span));
        };
        let labels: Vec<String> = items
            .iter()
            .filter_map(|item| item.as_string().map(str::to_owned))
            .collect();
        match EnumType::new(labels) {
            Ok(en_ty) => Node::Scalar(ScalarField {
                ty: Type::Enum(en_ty),
                computed: None,
                default: None,
                normalize: None,
                checks: Vec::new(),
                unique: false,
                span: en.value.span,
            }),
            Err(_) => {
                reporter.reject_hint(
                    en.value.span,
                    code::ENUM,
                    "`$enum` labels must be distinct (§5.9)",
                    "remove the repeated label",
                );
                Node::Scalar(placeholder(en.value.span))
            }
        }
    }

    fn like_node(&mut self, reporter: &mut Reporter, value: &DocValue, path: &[String]) -> Node {
        // `$like: "^"` is positional recursion (§5.8). Resolving the lexical
        // target to a named shape is a documented CORE seam; the nearest named
        // ancestor on `path` is used when available.
        let _ = value;
        match path.iter().rev().find(|seg| self.type_names.contains(*seg)) {
            Some(name) => Node::Named(name.clone()),
            None => {
                reporter.reject_hint(
                    value.span,
                    code::TYPE,
                    "`$like` positional recursion is only resolved against a `$types` shape in this pass",
                    "declare the recursive shape under `$types` and reference it by name",
                );
                Node::Scalar(placeholder(value.span))
            }
        }
    }

    /// The expanded field form `{ $type, $optional, $default, $normalize,
    /// $check, $unique }` (§5.1, A.3).
    fn expanded_field(&mut self, reporter: &mut Reporter, value: &DocValue) -> Node {
        let mut field = placeholder(value.span);
        let mut blob_member: Option<liasse_diag::ByteSpan> = None;
        for member in value.as_object().unwrap_or(&[]) {
            match member.name.text.as_str() {
                "$type" => {
                    if let Some(text) = member.value.as_string() {
                        match TypeParser::parse(text.trim(), &self.named) {
                            Ok(ty) => field.ty = ty,
                            Err(reason) => reporter.reject(member.value.span, code::TYPE, reason),
                        }
                    }
                }
                "$optional" => {
                    if member.value.as_bool() == Some(true)
                        && !matches!(field.ty, Type::Optional(_))
                    {
                        field.ty = Type::Optional(Box::new(std::mem::replace(&mut field.ty, Type::Json)));
                    }
                }
                "$default" => field.default = Some(default_source(&member.value)),
                "$normalize" => field.normalize = Some(expr_source(&member.value)),
                "$check" => field.checks = self.checks(reporter, &member.value),
                "$unique" => field.unique = member.value.as_bool() == Some(true),
                // Accepted-blob-type members (§18.2); only valid on `blob`.
                "$max_bytes" => {
                    blob_member.get_or_insert(member.value.span);
                    crate::blob::check_max_bytes(reporter, &member.value);
                }
                "$media" => {
                    blob_member.get_or_insert(member.value.span);
                    crate::blob::check_media(reporter, &member.value);
                }
                // §4.4: a timestamp precision override is one of a fixed set.
                "$precision" => {
                    let supported = member
                        .value
                        .as_string()
                        .is_some_and(|p| matches!(p, "s" | "ms" | "us" | "ns"));
                    if !supported {
                        reporter.reject_hint(
                            member.value.span,
                            code::TYPE,
                            "`$precision` must be one of `s`, `ms`, `us`, `ns` (§4.4)",
                            "use a supported timestamp precision",
                        );
                    }
                }
                // Migration mapping members (§20.1), typed by the migration phase.
                "$from" | "$as" | "$back" => {}
                other => reporter.reject(
                    member.span,
                    code::RESERVED_MEMBER,
                    format!("`{other}` is not an expanded-field member"),
                ),
            }
        }
        if let Some(span) = blob_member {
            crate::blob::require_blob_type(reporter, &field.ty, span);
        }
        Node::Scalar(field)
    }

    /// Parse a `$check` value: a bare expression, `[expr, message]`, or a list
    /// of `[expr, message]` pairs (§8.8).
    pub(super) fn checks(&self, reporter: &mut Reporter, value: &DocValue) -> Vec<Check> {
        if let Some(text) = value.as_string() {
            let _ = text;
            return vec![Check {
                condition: expr_source(value),
                message: None,
            }];
        }
        let Some(items) = value.as_array() else {
            reporter.reject(value.span, code::EXPR, "`$check` is an expression, `[expr, message]`, or a list of pairs");
            return Vec::new();
        };
        // `[expr, message]` is a single check when the first item is a string
        // and the second is a string message.
        if let [first, second] = items
            && first.as_string().is_some()
            && second.as_string().is_some()
        {
            return vec![Check {
                condition: expr_source(first),
                message: second.as_string().map(str::to_owned),
            }];
        }
        items.iter().map(|item| self.check_pair(reporter, item)).collect()
    }

    fn check_pair(&self, reporter: &mut Reporter, item: &DocValue) -> Check {
        if let Some(text) = item.as_string() {
            let _ = text;
            return Check {
                condition: expr_source(item),
                message: None,
            };
        }
        if let Some([cond, msg]) = item.as_array().and_then(|a| <&[_; 2]>::try_from(a).ok()) {
            return Check {
                condition: expr_source(cond),
                message: msg.as_string().map(str::to_owned),
            };
        }
        reporter.reject(item.span, code::EXPR, "each `$check` entry is `[expression, message]`");
        Check {
            condition: expr_source(item),
            message: None,
        }
    }

}
