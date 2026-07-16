//! Scalar-field construction (SPEC.md §5.1, §5.9, A.3): the string forms
//! (`"T"`, `"T?"`, `"T = default"`, `"= expr"`, a bare `$types` name), the
//! expanded `{ $type, ... }` field, enums, and the `$check` forms. Object-form
//! dispatch and the non-scalar nodes live in [`super::shapes`], keyed
//! collections in [`super::keys`], the shape-walking core in [`super`].
//! Continues the same [`Builder`] impl.

use liasse_syntax::{DocMember, DocValue};
use liasse_value::{EnumType, Type};

use crate::doc::DocValueExt;
use crate::report::{code, Reporter};
use crate::state::{Check, ExprSource, Node, ScalarField};
use crate::types::TypeParser;

use super::{default_source, expr_source, placeholder, Builder};

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

    pub(super) fn enum_node(&mut self, reporter: &mut Reporter, en: &DocMember) -> Node {
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

    /// The expanded field form `{ $type, $optional, $default, $normalize,
    /// $check, $unique }` (§5.1, A.3).
    pub(super) fn expanded_field(&mut self, reporter: &mut Reporter, value: &DocValue) -> Node {
        let mut field = placeholder(value.span);
        let mut blob_member: Option<liasse_diag::ByteSpan> = None;
        // §5.1: an expanded field's members all refine one field and "their
        // source order has no semantic effect". The base type (`$type`) and the
        // optional wrapper (`$optional`) are therefore gathered independently and
        // combined once after the loop, so `$optional` before `$type` cannot be
        // overwritten by a later `$type` assignment (which would silently drop
        // optionality and let an optional field be used as a `$key`).
        let mut base_ty = Type::Json;
        let mut optional = false;
        for member in value.as_object().unwrap_or(&[]) {
            match member.name.text.as_str() {
                "$type" => {
                    if let Some(text) = member.value.as_string() {
                        match TypeParser::parse(text.trim(), &self.named) {
                            Ok(ty) => base_ty = ty,
                            Err(reason) => reporter.reject(member.value.span, code::TYPE, reason),
                        }
                    }
                }
                "$optional" => optional = member.value.as_bool() == Some(true),
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
        field.ty = if optional && !matches!(base_ty, Type::Optional(_)) {
            Type::Optional(Box::new(base_ty))
        } else {
            base_ty
        };
        if let Some(span) = blob_member {
            crate::blob::require_blob_type(reporter, &field.ty, span);
        }
        Node::Scalar(field)
    }

    /// Parse a `$check` value: a bare expression, `[expr, message]`, or a list
    /// of `[expr, message]` pairs (§8.8).
    pub(super) fn checks(&self, reporter: &mut Reporter, value: &DocValue) -> Vec<Check> {
        if value.as_string().is_some() {
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
        if item.as_string().is_some() {
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
