//! Phase 1: the structural build (SPEC.md §5, Annex C.2/C.3).
//!
//! Walks the `$model` object (and `$types`) into the owned [`Shape`] tree,
//! enforcing every rule that needs only local structure: the §2.5 name grammar,
//! reserved/unknown members, `$key` naming declared key-eligible fields (§5.4,
//! A.8), `$unique` shape, enum distinctness (§5.9), and the set/ref/struct
//! forms. Expression-typed rules (defaults, checks, refs' target existence,
//! mutations, surfaces) are left to later phases, which read the raw sources
//! collected here.
//!
//! Member-node construction is split by concern: scalar-field forms
//! (`"T = default"`, `$enum`, expanded fields, `$check`) in [`fields`], keyed
//! collections (`$key`/`$unique`) in [`keys`], and object-form dispatch with
//! the non-scalar nodes (structs, sets, views, refs, opaque feature
//! declarations) in [`shapes`].

mod fields;
mod keys;
mod shapes;
mod sort;

use std::collections::BTreeMap;

use liasse_syntax::{DocMember, DocValue};
use liasse_value::Type;

use crate::doc::DocValueExt;
use crate::names::{is_reserved, DeclName};
use crate::report::{code, Reporter};
use crate::state::{ExprSource, Member, Node, ScalarField, Shape};
use crate::types::NamedTypes;

/// A `$mut` declaration awaiting expression validation, with its receiver path.
pub(crate) struct RawMut<'a> {
    /// The receiver location from the model root (empty = root mutation).
    pub path: Vec<String>,
    /// The raw member name (may carry a `name({ proto })` prototype).
    pub name: String,
    /// The bytes of the mutation member.
    pub span: liasse_diag::ByteSpan,
    /// The program body: a statement string or an array of statement strings.
    pub body: &'a DocValue,
}

/// A `$public`/`$roles` member awaiting expression validation.
pub(crate) struct RawSurface<'a> {
    /// The receiver location from the model root where the block is declared
    /// (empty = root). A nested `$roles` scopes its `.` and mutation references
    /// to its containing row (§10.3).
    pub path: Vec<String>,
    /// Whether the surfaces are public (unauthenticated) or role-scoped.
    pub public: bool,
    /// The `$public` or `$roles` object.
    pub value: &'a DocValue,
}

/// A reserved feature declaration awaiting a later validation phase, tagged with
/// the receiver path (from the model root) whose row scope it is checked in.
///
/// The feature phases (`auth`, `bucket`, `meter`, `blob`) share this raw
/// carrier: each collects the located `$`-member value plus the enclosing
/// shape/collection path, then locates the built row type by that path exactly
/// as the mutation phase does (SPEC.md §14/§15/§11/§18 static rules).
pub(crate) struct RawDecl<'a> {
    /// The enclosing receiver location from the model root.
    pub path: Vec<String>,
    /// The bytes of the whole member (for a span-anchored diagnostic).
    pub span: liasse_diag::ByteSpan,
    /// The `$`-member value.
    pub value: &'a DocValue,
}

/// The output of phase 1: the data tree plus raw behaviour to validate.
pub(crate) struct StateBuild<'a> {
    pub root: Shape,
    pub types: BTreeMap<String, Node>,
    pub raw_muts: Vec<RawMut<'a>>,
    pub surfaces: Vec<RawSurface<'a>>,
    pub auths: Vec<RawDecl<'a>>,
    pub buckets: Vec<RawDecl<'a>>,
    pub limits: Vec<RawDecl<'a>>,
    pub consumes: Vec<RawDecl<'a>>,
    pub blob_storage: Vec<RawDecl<'a>>,
    /// Absolute paths (`/segment/...`) of source-backed bucket collections,
    /// whose rows are read-only (§14.4).
    pub source_buckets: Vec<String>,
    /// Source-backed bucket collection declarations (§14.4–§14.6): the whole
    /// collection object (its `$bucket`, `$key`, and output-field members), typed
    /// into a temporal-collection row by [`crate::bucket::type_source_buckets`].
    pub source_bucket_decls: Vec<RawDecl<'a>>,
    /// Module spaces (§13.2, §13.8): the model path of each `$modules` node and
    /// the per-instance shape built from its `$interfaces`. The deferred
    /// [`crate::module::type_module_spaces`] pass projects each instance shape and
    /// writes the instance-name-keyed view row onto the placeholder view node.
    pub module_spaces: Vec<(Vec<String>, Shape)>,
    /// A module package's top-level `$config` struct (§13.1), when declared: the
    /// immutable typed struct of installation values built as a static struct
    /// shape. `None` for an application or a module with no `$config`. Resolved to
    /// a [`ConfigSchema`](crate::ConfigSchema) and retained on the model.
    pub config: Option<Shape>,
}

/// The structural builder: accumulates the reusable type table and the raw
/// behaviour lists as it walks.
pub(crate) struct Builder<'a> {
    named: NamedTypes,
    /// Every declared `$types` name, known up front so a self-referential shape
    /// (`subcompanies: "company"`) resolves while its own body is still being
    /// built.
    type_names: std::collections::BTreeSet<String>,
    types: BTreeMap<String, Node>,
    raw_muts: Vec<RawMut<'a>>,
    surfaces: Vec<RawSurface<'a>>,
    auths: Vec<RawDecl<'a>>,
    buckets: Vec<RawDecl<'a>>,
    limits: Vec<RawDecl<'a>>,
    consumes: Vec<RawDecl<'a>>,
    blob_storage: Vec<RawDecl<'a>>,
    source_buckets: Vec<String>,
    source_bucket_decls: Vec<RawDecl<'a>>,
    module_spaces: Vec<(Vec<String>, Shape)>,
    /// Model-root paths a `$like: "^"` positional recursion resolves to (§5.8):
    /// the containing shape/collection each inline recursive field adopts. After
    /// the root is built, each target's node is registered in [`Self::types`]
    /// under its absolute-path key so the resolver projects it lazily (like a
    /// `$types` name), with the resolver's depth cap terminating the recursion.
    recur_targets: std::collections::BTreeSet<Vec<String>>,
}

impl<'a> Builder<'a> {
    /// Build the whole state tree from the located `$model` (and `$types`), plus
    /// a module package's `$config` struct schema (§13.1) when present.
    pub(crate) fn run(
        reporter: &mut Reporter,
        model: &'a DocValue,
        types_doc: Option<&'a DocValue>,
        config_doc: Option<&'a DocValue>,
    ) -> StateBuild<'a> {
        let mut builder = Builder {
            named: NamedTypes::new(),
            type_names: std::collections::BTreeSet::new(),
            types: BTreeMap::new(),
            raw_muts: Vec::new(),
            surfaces: Vec::new(),
            auths: Vec::new(),
            buckets: Vec::new(),
            limits: Vec::new(),
            consumes: Vec::new(),
            blob_storage: Vec::new(),
            source_buckets: Vec::new(),
            source_bucket_decls: Vec::new(),
            module_spaces: Vec::new(),
            recur_targets: std::collections::BTreeSet::new(),
        };
        if let Some(types_doc) = types_doc {
            builder.build_types(reporter, types_doc);
        }
        let root = match model.as_object() {
            Some(members) => builder.build_shape(reporter, members, &[], true),
            None => {
                reporter.reject(model.span, code::HEADER, "`$model` must be an object");
                Shape::default()
            }
        };
        builder.register_recursion_targets(&root);
        // §13.1: a module's `$config` is a static struct of typed installation
        // values, built after the root so it may reference the same `$types`.
        let config = config_doc.map(|doc| builder.config_shape(reporter, doc));
        StateBuild {
            root,
            types: builder.types,
            raw_muts: builder.raw_muts,
            surfaces: builder.surfaces,
            auths: builder.auths,
            buckets: builder.buckets,
            limits: builder.limits,
            consumes: builder.consumes,
            blob_storage: builder.blob_storage,
            source_buckets: builder.source_buckets,
            source_bucket_decls: builder.source_bucket_decls,
            module_spaces: builder.module_spaces,
            config,
        }
    }

    /// Build a module package's `$config` struct schema (§13.1): the immutable
    /// typed struct whose members are the installation values a child's
    /// expressions read through `$config`. Each member must be a typed *value*
    /// field — a scalar, struct, set, or ref — so a reserved `$`-member or a
    /// view/keyed-collection member is rejected. That, together with the
    /// per-member type parse `data_member` performs, is the static "valid struct
    /// type" check (§13.1). The resulting shape is resolved to a struct row the
    /// runtime type-checks install values against and binds as `$config`.
    fn config_shape(&mut self, reporter: &mut Reporter, config: &'a DocValue) -> Shape {
        // A non-object `$config` is already reported by the header phase.
        let Some(members) = config.as_object() else {
            return Shape::default();
        };
        let mut shape = Shape::default();
        for member in members {
            if is_reserved(&member.name.text) {
                reporter.reject_hint(
                    member.span,
                    code::MODULE,
                    format!(
                        "`{}` is not a `$config` value; `$config` declares a struct of typed installation fields (§13.1)",
                        member.name.text
                    ),
                    "declare each config value as `name: \"type\"` or `name: \"type = default\"`",
                );
                continue;
            }
            let index = shape.members.len();
            self.data_member(reporter, member, &["$config".to_owned()], &mut shape);
            if let Some(added) = shape.members.get(index)
                && matches!(added.node, Node::View(_) | Node::Collection(_))
            {
                reporter.reject_hint(
                    member.span,
                    code::MODULE,
                    format!(
                        "`$config.{}` must be a typed value field, not a view or collection (§13.1)",
                        member.name.text
                    ),
                    "declare a scalar, struct, set, or ref installation value",
                );
            }
        }
        shape
    }

    /// Build the reusable `$types` shapes (§5.8). Scalar-shaped entries (enums,
    /// structs, primitive aliases) also feed the type parser so a bare name in a
    /// field position resolves; every entry feeds the node table so a named
    /// collection shape resolves lazily.
    fn build_types(&mut self, reporter: &mut Reporter, types_doc: &'a DocValue) {
        let Some(members) = types_doc.as_object() else {
            reporter.reject(types_doc.span, code::HEADER, "`$types` must be an object");
            return;
        };
        for member in members {
            self.type_names.insert(member.name.text.clone());
        }
        for member in members {
            if let Err(reason) = DeclName::parse(&member.name.text) {
                reporter.reject(member.name.span, code::NAME_GRAMMAR, reason);
                continue;
            }
            let node = self.member_node(reporter, member, &["$types".to_owned()]);
            if let Node::Scalar(field) = &node
                && field.is_writable()
                && field.default.is_none()
            {
                self.named.insert(member.name.text.clone(), field.ty.clone());
            }
            self.types.insert(member.name.text.clone(), node);
        }
    }

    /// Build a struct/root body: validate every member and collect its node.
    fn build_shape(
        &mut self,
        reporter: &mut Reporter,
        members: &'a [DocMember],
        path: &[String],
        is_root: bool,
    ) -> Shape {
        let mut shape = Shape::default();
        for member in members {
            let name = member.name.text.as_str();
            if is_reserved(name) {
                self.shape_reserved(reporter, member, path, is_root, &mut shape);
            } else {
                self.data_member(reporter, member, path, &mut shape);
            }
        }
        shape
    }

    /// Handle a `$`-prefixed member at the shape level.
    fn shape_reserved(
        &mut self,
        reporter: &mut Reporter,
        member: &'a DocMember,
        path: &[String],
        is_root: bool,
        shape: &mut Shape,
    ) {
        match member.name.text.as_str() {
            // $key/$unique are consumed where the collection is recognised.
            "$key" | "$unique" => {}
            // §20.1: a collection MAY carry `$from` to rename an old collection,
            // adopting its rows ("The same shorthand renames a collection"). This
            // is the collection-level analogue of the field-level `$from`/`$as`/
            // `$back` mapping members accepted structurally by the field builder;
            // the two-model migration phase (a documented seam) types it. Accept
            // it structurally here so a rename target is not rejected as an
            // unknown reserved member.
            "$from" => {}
            "$bucket" => self.buckets.push(RawDecl {
                path: path.to_vec(),
                span: member.span,
                value: &member.value,
            }),
            "$consumes" => self.consumes.push(RawDecl {
                path: path.to_vec(),
                span: member.span,
                value: &member.value,
            }),
            // §7.3: a collection or view MAY declare `$sort`. Its keys resolve
            // against the row shape at runtime; here its form is validated.
            "$sort" => sort::check(reporter, &member.value),
            "$check" => shape.checks.extend(self.checks(reporter, &member.value)),
            "$mut" => self.collect_muts(reporter, &member.value, path),
            "$public" if is_root => self.surfaces.push(RawSurface {
                path: path.to_vec(),
                public: true,
                value: &member.value,
            }),
            "$roles" => self.surfaces.push(RawSurface {
                path: path.to_vec(),
                public: false,
                value: &member.value,
            }),
            "$auth" => self.auths.push(RawDecl {
                path: path.to_vec(),
                span: member.span,
                value: &member.value,
            }),
            "$limits" => {
                // Record each declared meter name so the resolver can expose
                // its §15.6 accessor field on this row's shape; the meter's own
                // static validation is deferred to `meter::check`.
                if let Some(meters) = member.value.as_object() {
                    shape
                        .meters
                        .extend(meters.iter().map(|m| m.name.text.clone()));
                }
                self.limits.push(RawDecl {
                    path: path.to_vec(),
                    span: member.span,
                    value: &member.value,
                });
            }
            "$blob_storage" => self.blob_storage.push(RawDecl {
                path: path.to_vec(),
                span: member.span,
                value: &member.value,
            }),
            other => reporter.reject_hint(
                member.span,
                code::RESERVED_MEMBER,
                format!("`{other}` begins with the reserved `$` prefix but is not a declaration here"),
                "remove it; only defined `$` declarations are allowed in a shape",
            ),
        }
    }

    /// Handle an application-named data member: validate its name and build it.
    fn data_member(
        &mut self,
        reporter: &mut Reporter,
        member: &'a DocMember,
        path: &[String],
        shape: &mut Shape,
    ) {
        let name = match DeclName::parse(&member.name.text) {
            Ok(name) => name,
            Err(reason) => {
                reporter.reject(member.name.span, code::NAME_GRAMMAR, reason);
                return;
            }
        };
        let mut child_path = path.to_vec();
        child_path.push(member.name.text.clone());
        let node = self.member_node(reporter, member, &child_path);
        shape.members.push(Member {
            name,
            span: member.span,
            node,
        });
    }

    /// Build one member's node from its value form (Annex C.3).
    fn member_node(
        &mut self,
        reporter: &mut Reporter,
        member: &'a DocMember,
        path: &[String],
    ) -> Node {
        match &member.value {
            v if v.as_string().is_some() => {
                self.scalar_from_string(reporter, v, v.span)
            }
            v if v.as_object().is_some() => self.object_node(reporter, member, path),
            other => {
                reporter.reject_hint(
                    other.span,
                    code::TYPE,
                    format!("a declaration value must be a type string or an object, found {}", other.kind_name()),
                    "declare a field as `\"text\"` or expand it into an object",
                );
                Node::Scalar(placeholder(other.span))
            }
        }
    }

    /// Collect a `$mut` map's members as raw mutations for the mutation phase.
    fn collect_muts(&mut self, reporter: &mut Reporter, value: &'a DocValue, path: &[String]) {
        let Some(members) = value.as_object() else {
            reporter.reject(value.span, code::MUTATION, "`$mut` maps names to mutation programs");
            return;
        };
        for member in members {
            self.raw_muts.push(RawMut {
                path: path.to_vec(),
                name: member.name.text.clone(),
                span: member.span,
                body: &member.value,
            });
        }
    }
}

impl Builder<'_> {
    /// Record the model-root path a `$like: "^"` field adopts (§5.8), returning
    /// the absolute-path key under which its containing shape is registered so
    /// the field can be represented as a lazily-resolved `Node::Named`.
    pub(super) fn recursion_target(&mut self, target: Vec<String>) -> String {
        let key = absolute_path(&target);
        self.recur_targets.insert(target);
        key
    }

    /// After the root is built, register each `$like: "^"` target shape's node in
    /// the type table under its absolute-path key. The registered node is the
    /// fully-built containing collection/struct, so its own recursive fields
    /// (themselves `Node::Named` keys) resolve lazily and the resolver's depth
    /// cap terminates the expansion.
    fn register_recursion_targets(&mut self, root: &Shape) {
        for target in std::mem::take(&mut self.recur_targets) {
            if let Some(node) = node_at_path(root, &target) {
                self.types.insert(absolute_path(&target), node.clone());
            }
        }
    }
}

/// The node declared at a model-root path (`["categories"]` → the `categories`
/// collection node), descending through collection/struct bodies for every
/// segment before the last.
fn node_at_path<'s>(root: &'s Shape, path: &[String]) -> Option<&'s Node> {
    let (last, parents) = path.split_last()?;
    let mut shape = root;
    for segment in parents {
        shape = match &shape.member(segment)?.node {
            Node::Collection(collection) => &collection.shape,
            Node::Struct(inner) => inner,
            _ => return None,
        };
    }
    shape.member(last).map(|member| &member.node)
}

/// A blank writable-`json` field used as a placeholder after a rejection so the
/// tree stays shaped and later phases can keep going.
fn placeholder(span: liasse_diag::ByteSpan) -> ScalarField {
    ScalarField {
        ty: Type::Json,
        computed: None,
        default: None,
        normalize: None,
        checks: Vec::new(),
        unique: false,
        span,
    }
}

/// The absolute `/segment/...` index form of a receiver path, matching the
/// collection index built by [`crate::refs`].
fn absolute_path(path: &[String]) -> String {
    let mut out = String::new();
    for segment in path {
        out.push('/');
        out.push_str(segment);
    }
    out
}

fn expr_source(value: &DocValue) -> ExprSource {
    ExprSource {
        text: value.as_string().unwrap_or_default().to_owned(),
        span: value.span,
    }
}

/// An expanded `$default` (a literal-or-expression position, SPEC.md §4.2): a
/// string beginning with `=` is an expression, so the marker is stripped before
/// the expression is parsed. Strings without the marker are left verbatim (the
/// bare-expression / `T = default` shorthand already carries no marker).
fn default_source(value: &DocValue) -> ExprSource {
    let raw = value.as_string().unwrap_or_default();
    match raw.trim_start().strip_prefix('=') {
        Some(rest) => ExprSource {
            text: rest.trim().to_owned(),
            span: value.span,
        },
        None => expr_source(value),
    }
}
