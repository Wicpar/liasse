//! Object-form member dispatch (SPEC.md Annex C.2) and the non-scalar node
//! forms: static structs, sets, views, refs, `$like` recursion, the `$keyring`
//! version-view declaration (projected as a typed view for its feature phase),
//! and the `$bucket` source-collection declaration. Scalar-field forms live in
//! [`super::fields`], keyed collections — including the map form a module
//! collection takes — in [`super::keys`]. Continues the same [`Builder`] impl.

use liasse_syntax::{DocMember, DocValue};
use liasse_value::Type;

use crate::doc::DocValueExt;
use crate::names::DeclName;
use crate::report::{code, Reporter};
use crate::state::{ExprSource, Member, Node, Reference, SetField, Shape};

use super::closed::{self, closed_decl};
use super::{absolute_path, placeholder, Builder};

impl<'a> Builder<'a> {
    /// The mutually-exclusive node-kind shape markers (Annex C.2, SPEC-ISSUES
    /// 25). Exactly one fixes an object's node kind; `$bucket` is deliberately
    /// absent because it COMPOSES with `$key` (a keyed lifecycle collection) and
    /// otherwise declares a source-backed bucket, so it never conflicts here.
    const KIND_MARKERS: &'static [&'static str] =
        &["$key", "$set", "$view", "$ref", "$enum", "$type", "$keyring", "$like"];

    /// Dispatch an object-valued member on its shape marker (Annex C.2).
    pub(super) fn object_node(
        &mut self,
        reporter: &mut Reporter,
        member: &'a DocMember,
        path: &[String],
    ) -> Node {
        let value = &member.value;
        // SPEC-ISSUES 25 / Annex C.2: an object's node kind is fixed by exactly
        // one kind marker. Two mutually-exclusive markers leave the kind
        // undetermined — reject with a marker-aware diagnostic that names them,
        // rather than letting the first-checked marker silently win and the loser
        // surface (or not) as an incidental "unexpected member". The dispatch
        // below still runs so one node is returned, but the recorded rejection
        // fails the load.
        let present: Vec<&str> =
            Self::KIND_MARKERS.iter().copied().filter(|m| value.member(m).is_some()).collect();
        // The members these two guards reject by name, so the per-form §2.5
        // closed-vocabulary check below does not report the same mistake twice.
        let mut reported: Vec<&str> = Vec::new();
        // §5.4/C.2: `$value` COMPOSES with `$key` to put a declaration in map
        // form; beside any other kind marker it has no meaning, and beside none
        // at all it names no key type. Both are the same static error the
        // conflicting-marker rule above raises, named the same way, so a `$value`
        // never silently degrades the object to some other kind.
        if value.member("$value").is_some() {
            reported.push("$value");
            let conflict = present.iter().copied().find(|m| *m != "$key");
            if let Some(other) = conflict {
                reporter.reject_hint(
                    value.span,
                    code::SHAPE,
                    format!(
                        "conflicting shape markers `$value` and `{other}` on one object: `$value` \
                         composes only with `$key`, as a map (§5.4, Annex C.2)"
                    ),
                    "split the map into its own declaration",
                );
            } else if !present.contains(&"$key") {
                reporter.reject_hint(
                    value.span,
                    code::SHAPE,
                    "`$value` needs a `$key`: it is what puts a declaration in map form, and a \
                     map's entries are keyed (§5.4)",
                    "write `{ \"$key\": \"<key type>\", \"$value\": \"<value type>\" }`",
                );
            }
        }
        if present.len() > 1 {
            reported.extend(present.iter().copied());
            reporter.reject_hint(
                value.span,
                code::SHAPE,
                format!(
                    "conflicting shape markers {} on one object: an object's node kind is fixed by \
                     exactly one of {} (Annex C.2)",
                    present.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(" and "),
                    Self::KIND_MARKERS.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", "),
                ),
                "keep exactly one kind marker; split the others into separate declarations",
            );
        }
        if value.member("$keyring").is_some() {
            closed_decl(reporter, value, &reported, &closed::KEYRING);
            return self.keyring_node(reporter, value);
        }
        // §5.3/§14.4/§14.6: a source-backed bucket (its `$bucket` object declares
        // a `$source`) derives its rows and MAY carry a custom `$key` built from
        // its structural bindings (`$source.external_id`, `$from`); it routes to
        // the source-bucket node even alongside a `$key`, so the ordinary
        // keyed-collection `$key` validation (which knows only declared fields)
        // never falsely rejects those bindings. A `$bucket` without a `$key` is
        // likewise source-backed. A *lifecycle* bucket (§14.1) has no `$source`;
        // it composes `$bucket` with an ordinary `$key` and builds as a keyed
        // collection through the `$key` branch below.
        if value.member("$bucket").is_some()
            && (source_backed(value) || value.member("$key").is_none())
        {
            closed_decl(reporter, value, &reported, &closed::SOURCE_BUCKET);
            return self.source_bucket_node(value, path);
        }
        if value.member("$key").is_some() {
            // §5.4: `$value` alongside `$key` is the MAP form — `$key` reads as a
            // type expression rather than as declared field names, and the row
            // shape is the fixed `{ $key, $value }`. It is the same
            // `Node::Collection`, built from a synthesized two-member shape, so
            // every keyed-collection path downstream applies unchanged.
            return Node::Collection(Box::new(match value.member("$value") {
                Some(entry) => self.map_collection(reporter, value, entry, path),
                None => self.collection(reporter, value, path),
            }));
        }
        if let Some(set) = value.member("$set") {
            closed_decl(reporter, value, &reported, &closed::SET);
            return self.set_node(reporter, value, set);
        }
        if let Some(view) = value.member("$view") {
            closed_decl(reporter, value, &reported, &closed::VIEW);
            return self.view_node(view);
        }
        if value.member("$ref").is_some() {
            closed_decl(reporter, value, &reported, &closed::REF);
            return self.ref_node(reporter, value);
        }
        if let Some(en) = value.member("$enum") {
            // §5.1/§5.9: a field-level inline enum that also carries an
            // expanded-field refinement ($default/$optional/$normalize/$check/
            // $unique/$precision) is an expanded field whose base type is the
            // inline enum; a bare `{ $enum: [...] }` keeps the fast enum-node path.
            const REFINEMENTS: &[&str] =
                &["$default", "$optional", "$normalize", "$check", "$unique", "$precision"];
            if REFINEMENTS.iter().any(|m| value.member(m).is_some()) {
                return self.expanded_field(reporter, value);
            }
            closed_decl(reporter, value, &reported, &closed::ENUM);
            return self.enum_node(reporter, en);
        }
        if value.member("$like").is_some() {
            closed_decl(reporter, value, &reported, &closed::LIKE);
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
    /// therefore projected as a view whose row carries the §17.2-pinned version
    /// members ([`keyring_version_row`]) — so an ordinary projection, `$sort`, or
    /// `$key` group over `algorithm`/`created_at`/… type-checks and loads (§7),
    /// matching the metadata the runtime's version rows materialize. The view's
    /// stand-in expression `.` is never the ring's value — [`crate::resolve`]
    /// takes the view row directly — it only keeps the expression checker's
    /// well-formedness pass satisfied for a synthetic, non-authored view.
    /// The declaration object's own closed vocabulary (nothing beside
    /// `$keyring`) is enforced by [`closed_decl`] at the dispatch site, with
    /// every other object form's.
    fn keyring_node(&self, reporter: &mut Reporter, value: &DocValue) -> Node {
        if let Some(keyring) = value.member("$keyring") {
            crate::keyring::check(reporter, &keyring.value);
        }
        Node::View(crate::state::ViewDecl {
            expr: ExprSource {
                text: ".".to_owned(),
                span: value.span,
            },
            row: keyring_version_row(),
        })
    }

    /// Add one read member per declared `$interfaces` entry to a module
    /// collection's row shape (§13.8): the interface name, its node the
    /// interface's `$view` row shape. The `$mut` contracts are boundary *call*
    /// contracts, not readable state, so only `$view` contributes to the read
    /// shape.
    ///
    /// The row this lands on is the ordinary `{ $key, $value }` map row of the
    /// module collection, so `modules.$key` is the map key, `.modules[@id].$value`
    /// is the module value, and `.modules::iface` is the §6.4 nested traversal
    /// every collection has — no separate addressing scheme of its own.
    pub(super) fn module_interface_members(
        &mut self,
        reporter: &mut Reporter,
        interfaces: Option<&'a DocMember>,
        path: &[String],
        shape: &mut Shape,
    ) {
        let Some(interfaces) = interfaces.and_then(|m| m.value.as_object()) else {
            return;
        };
        for interface in interfaces {
            let Ok(name) = DeclName::parse(&interface.name.text) else {
                continue;
            };
            let Some(view) = interface.value.member("$view") else {
                continue;
            };
            let mut iface_path = path.to_vec();
            iface_path.push(interface.name.text.clone());
            let node = self.interface_node(reporter, &view.value, &iface_path);
            shape.members.push(Member {
                name,
                span: interface.span,
                node,
            });
        }
    }

    /// The node of one module-collection interface's `$view` shape (§13.8). With
    /// a `$key` it is a keyed collection of interface rows; without one it is a
    /// single struct row.
    fn interface_node(&mut self, reporter: &mut Reporter, view: &'a DocValue, path: &[String]) -> Node {
        if view.member("$key").is_some() {
            Node::Collection(Box::new(self.collection(reporter, view, path)))
        } else if let Some(members) = view.as_object() {
            Node::Struct(self.build_shape(reporter, members, path, false))
        } else {
            Node::Scalar(placeholder(view.span))
        }
    }

    fn set_node(&mut self, reporter: &mut Reporter, value: &DocValue, set: &DocMember) -> Node {
        // A set of refs declares `$set: { $ref: ... }`. Keep the full member
        // reference (target + `$on_delete`) on the set field so §21.1 governs each
        // member exactly like a scalar ref (§5.6), instead of flattening it to a
        // bare element type. Other object element shapes are a documented CORE seam
        // (element must be a scalar type).
        if set.value.member("$ref").is_some() {
            // The element object is a `$ref` declaration in its own right, and no
            // dispatcher judged its shape markers, so its whole §2.5 vocabulary is
            // checked here — an `$on_delete` element keeps its meaning, a `$check`
            // element is named rather than dropped.
            closed_decl(reporter, &set.value, &[], &closed::REF);
            if let Node::Reference(reference) = self.ref_node(reporter, &set.value) {
                return Node::Set(SetField {
                    element: Type::Ref(liasse_value::RefTarget::for_key(&reference.key_type)),
                    element_ref: Some(reference),
                    span: value.span,
                });
            }
        }
        let element = self.shape_or_type(reporter, &set.value);
        Node::Set(SetField {
            element,
            element_ref: None,
            span: value.span,
        })
    }

    /// Resolve a non-ref `$set` element type. §5.5: "the value of `$set` is the
    /// shape of every member" — any scalar member shape is admissible: a type
    /// string or an inline `{ $enum: [...] }` (§5.9), the same base-type
    /// vocabulary an expanded field's `$type` accepts. A `{ $ref: ... }` element
    /// is handled by the caller before this point.
    fn shape_or_type(&mut self, reporter: &mut Reporter, value: &DocValue) -> Type {
        let Some(element) = self.scalar_shape(reporter, value) else {
            reporter.reject_hint(
                value.span,
                code::TYPE,
                "a `$set` element must be a type, an inline `{ $enum: [...] }`, or a `{ $ref: ... }`",
                "e.g. `\"tags\": { \"$set\": \"text\" }`",
            );
            return Type::Json;
        };
        // §5.5 / A.1: a set element type is never `T?`. The string
        // `{ $set: T? }` is rejected in `map_type`; this catches the inline
        // `{ $set: "T?" }` element, whose optional is a top-level
        // `optional` that `map_type` cannot see as a set member.
        if matches!(element, Type::Optional(_)) {
            reporter.reject(value.span, code::TYPE, crate::types::set_optional_reason());
            return Type::Json;
        }
        element
    }

    /// The declaration object's closed vocabulary (nothing beside `$view`) is
    /// enforced by [`closed_decl`] at the dispatch site.
    fn view_node(&mut self, view: &DocMember) -> Node {
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

/// The §17.2 keyring version-metadata row shape: the members every managed
/// version exposes through the public keyring view (`.$current`/`.$accepted`/
/// `.$public`/`.$versions`). The types mirror the rows the runtime materializes
/// (`liasse-runtime::keyring_view::version_row`): the `int` version ordinal as
/// the row identity `id`, the `text` `algorithm`, the `bytes` `public_key`
/// material, the `timestamp` `created_at`, and the `?`-optional lifecycle stamps
/// (`activated_at`/`retired_at`/`revoked_at`) and `attestation`, each omitted
/// when absent (§A.9). Private key bytes and provider credentials never appear.
fn keyring_version_row() -> liasse_expr::RowType {
    use liasse_expr::ExprType;
    let ts = || ExprType::scalar(Type::timestamp());
    let opt_ts = || ExprType::scalar(Type::Optional(Box::new(Type::timestamp())));
    liasse_expr::RowType::new(
        [
            ("id".to_owned(), ExprType::scalar(Type::Int)),
            ("algorithm".to_owned(), ExprType::scalar(Type::Text)),
            ("public_key".to_owned(), ExprType::scalar(Type::Bytes)),
            ("created_at".to_owned(), ts()),
            ("activated_at".to_owned(), opt_ts()),
            ("retired_at".to_owned(), opt_ts()),
            ("revoked_at".to_owned(), opt_ts()),
            ("attestation".to_owned(), ExprType::scalar(Type::Optional(Box::new(Type::Bytes)))),
        ],
        Some(ExprType::scalar(Type::Int)),
    )
}

/// Whether an object declares a source-backed bucket (§14.4): a `$bucket` whose
/// value is an object carrying a `$source` view. This is the discriminator
/// between a source-backed bucket (which derives its rows and may carry a custom
/// `$key`, §14.6) and a lifecycle bucket (§14.1: an until-expression `$bucket`,
/// with no `$source`, that composes with an ordinary `$key`). The runtime uses
/// the same test to compile the bucket (`source_bucket.rs::is_source_bucket`).
fn source_backed(value: &DocValue) -> bool {
    value
        .member("$bucket")
        .and_then(|bucket| bucket.value.as_object())
        .is_some_and(|members| members.iter().any(|m| m.name.text == "$source"))
}
