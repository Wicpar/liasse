//! Object-form member dispatch (SPEC.md Annex C.2) and the non-scalar node
//! forms: static structs, sets, views, refs, `$like` recursion, and the opaque
//! `$keyring`/`$modules`/`$bucket` declarations collected for their feature
//! phases. Scalar-field forms live in [`super::fields`], keyed collections in
//! [`super::keys`]. Continues the same [`Builder`] impl.

use liasse_syntax::{DocMember, DocValue};
use liasse_value::Type;

use crate::doc::DocValueExt;
use crate::report::{code, Reporter};
use crate::state::{ExprSource, Node, Reference, SetField};

use super::{absolute_path, placeholder, Builder};

impl<'a> Builder<'a> {
    /// Dispatch an object-valued member on its shape marker (Annex C.2).
    pub(super) fn object_node(
        &mut self,
        reporter: &mut Reporter,
        member: &'a DocMember,
        path: &[String],
    ) -> Node {
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
    /// collection (§14.4/§14.6). Its rows are derived from a `$source` view and
    /// expose the source identity and interval bounds as structural bindings
    /// (`$source`/`$from`/`$until`/`$index`), plus the collection's own output
    /// fields (`plan: "= $source.plan"`). Those output-field types need the source
    /// scope the general tree walk does not carry, so the node is projected as a
    /// [temporal-collection view](crate::state::ViewDecl) whose row is *computed
    /// later* by [`crate::bucket::type_source_buckets`] — it runs before the tree
    /// and surface checks so a temporal selector over the collection type-checks.
    /// The whole collection object is recorded for that pass; the absolute path is
    /// recorded read-only for the mutation phase (§14.4).
    fn source_bucket_node(&mut self, value: &'a DocValue, path: &[String]) -> Node {
        self.source_buckets.push(absolute_path(path));
        self.source_bucket_decls.push(super::RawDecl {
            path: path.to_vec(),
            span: value.span,
            value,
        });
        // A placeholder empty temporal view; the real row lands in the pre-pass.
        Node::View(crate::state::ViewDecl {
            expr: ExprSource {
                text: ".".to_owned(),
                span: value.span,
            },
            row: liasse_expr::RowType::keyless(std::iter::empty::<(String, liasse_expr::ExprType)>()),
        })
    }

    /// A `$keyring` managed-keyring declaration (§17.1, C.16). Its policy shape
    /// is validated inline (no cross-model scope is needed); provider capability
    /// resolution is a documented runtime seam.
    ///
    /// §17.2: the runtime exposes the ring's managed versions as a *view* of
    /// version-metadata rows, so a keyring public selector
    /// (`.$current`/`.$accepted`/`.$public`/`.$versions`) resolves against a view
    /// rather than the opaque `json` a scalar placeholder would give. The node is
    /// therefore projected as a view; its row is open/empty because §17.2 pins no
    /// version-metadata member names (SPEC-ISSUES 18). The view's stand-in
    /// expression `.` is never the ring's value — [`crate::resolve`] takes the
    /// view row directly — it only keeps the expression checker's well-formedness
    /// pass satisfied for a synthetic, non-authored view.
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
        Node::View(crate::state::ViewDecl {
            expr: ExprSource {
                text: ".".to_owned(),
                span: value.span,
            },
            row: liasse_expr::RowType::keyless(std::iter::empty::<(String, liasse_expr::ExprType)>()),
        })
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
            return match crate::types::TypeParser::parse(text.trim(), &self.named) {
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
        // §5.5: "the value of `$set` is the shape of every member" — any scalar
        // member shape is admissible, including an inline enum type. An `{ $enum:
        // [...] }` element declares a set of enum labels (§5.9).
        if let Some(en) = value.member("$enum") {
            if let Node::Scalar(field) = self.enum_node(reporter, en) {
                return field.ty;
            }
            return Type::Json;
        }
        reporter.reject_hint(
            value.span,
            code::TYPE,
            "a `$set` element must be a type, an inline `{ $enum: [...] }`, or a `{ $ref: ... }`",
            "e.g. `\"tags\": { \"$set\": \"text\" }`",
        );
        Type::Json
    }

    fn view_node(&mut self, _reporter: &mut Reporter, view: &DocMember) -> Node {
        // A `$view` value is an expression; the optional leading `=` marker
        // (§4.2) is accepted and stripped, so a scalar/aggregate view such as
        // `"= size(.docs)"` reads the same as a bare `".docs { ... }"`.
        let raw = view.value.as_string().unwrap_or_default();
        let text = raw.trim_start().strip_prefix('=').map_or(raw, str::trim).to_owned();
        let expr = ExprSource {
            text,
            span: view.value.span,
        };
        crate::state::Node::View(crate::state::ViewDecl {
            expr,
            row: liasse_expr::RowType::keyless(std::iter::empty::<(String, liasse_expr::ExprType)>()),
        })
    }

    pub(super) fn ref_node(&mut self, reporter: &mut Reporter, value: &DocValue) -> Node {
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

    fn like_node(&mut self, reporter: &mut Reporter, value: &DocValue, path: &[String]) -> Node {
        // `$like: "^"` is positional recursion (§5.8): `^` names the immediately
        // containing shape, `^^` its parent, and so on. The field adopts that
        // shape's contract while keeping its own location and data.
        let target = value.member("$like").and_then(|m| m.value.as_string()).unwrap_or_default();
        let depth = target.chars().take_while(|c| *c == '^').count();
        if depth == 0 || depth != target.trim().chars().count() {
            reporter.reject_hint(
                value.span,
                code::TYPE,
                format!("`$like` names a lexical shape by `^` depth, not `{target}`"),
                "use `\"$like\": \"^\"` for the containing shape, `\"^^\"` for its parent",
            );
            return Node::Scalar(placeholder(value.span));
        }
        // A `$like` inside a named `$types` shape resolves against that name, so
        // the `$types` node table already carries its contract.
        if let Some(name) = path.iter().rev().find(|seg| self.type_names.contains(*seg)) {
            return Node::Named(name.clone());
        }
        // Otherwise the containing shape is a model-tree declaration: `^` drops
        // the field's own segment, each further `^` one more ancestor. Register
        // that shape's path so it is projected lazily from the node table.
        match path.len().checked_sub(depth).and_then(|cut| path.get(..cut)) {
            Some(target) => Node::Named(self.recursion_target(target.to_vec())),
            None => {
                reporter.reject_hint(
                    value.span,
                    code::TYPE,
                    format!("`$like: \"{target}\"` reaches above the model root"),
                    "reduce the `^` depth to a shape that contains this field",
                );
                Node::Scalar(placeholder(value.span))
            }
        }
    }
}
