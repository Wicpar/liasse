//! Installation `$config` resolution against a module's declared `$config` struct
//! (§13.1, §13.3).
//!
//! A module package declares an immutable typed `$config` struct; an installation
//! supplies values for its members. This module consumes the model's retained
//! [`ConfigSchema`](liasse_model::ConfigSchema) to
//!
//! * **type-check** each supplied value against the declared member type, and
//!   reject a member the struct does not declare or a value that does not decode
//!   to the declared type (§13.1/§13.3 "loading validates the configuration
//!   before the instance becomes active"), and
//! * **resolve** each omitted member from its declared default (§13.3 "an omitted
//!   installation `$config` uses package defaults"), rejecting a required member
//!   that was omitted,
//!
//! then assemble the resolved values into the `$config` structural [`Cell`] a
//! child's expressions read through `$config`/`$config.member` (§13.1).

use std::collections::BTreeMap;

use liasse_expr::{Cell, ExprType, Row, RowId};
use liasse_diag::SourceMap;
use liasse_model::{ConfigSchema, FieldDefault, LiteralDefault};
use liasse_value::Value;

use crate::compiled::compile_expr;
use crate::error::EngineError;
use crate::eval::EvalCtx;
use crate::scope::RuntimeScope;
use crate::state::Prospective;

/// The failure of binding an installation's `$config` onto an instance (§13.3):
/// either the supplied values do not satisfy the declared struct (a `$config`
/// mismatch, surfaced as an invalid install), or gathering the child state to
/// resolve a default faulted the store.
#[derive(Debug)]
pub(crate) enum ConfigBindError {
    /// The supplied `$config` does not match the declared struct (§13.1/§13.3).
    Mismatch(ConfigError),
    /// A store fault while gathering the child state to resolve a default.
    Engine(EngineError),
}

/// A failure to resolve an installation's `$config` against the declared struct
/// (§13.1/§13.3). Each carries enough context for the module host to surface a
/// `$config`-mismatch install rejection.
#[derive(Debug)]
pub(crate) enum ConfigError {
    /// A supplied member the declared `$config` struct does not declare (§13.1,
    /// §2.5: unknown declaration members are invalid).
    UnknownMember(String),
    /// A supplied member whose value does not decode to the declared member type
    /// (§13.3).
    TypeMismatch { member: String, detail: String },
    /// A required member (declaring no default) the installation omitted (§13.3).
    MissingRequired(String),
    /// A declared default that failed to compile or evaluate (§13.1).
    DefaultFailed { member: String, detail: String },
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownMember(name) => {
                write!(f, "member `{name}` is not declared by the module's typed `$config` struct")
            }
            Self::TypeMismatch { member, detail } => {
                write!(f, "value for `{member}` does not match the declared type: {detail}")
            }
            Self::MissingRequired(name) => {
                write!(f, "required member `{name}` was not supplied and declares no default")
            }
            Self::DefaultFailed { member, detail } => {
                write!(f, "default for `{member}` could not be resolved: {detail}")
            }
        }
    }
}

/// Type-check `supplied` against `schema` and fill each omitted member from its
/// default, returning the resolved `member → value` map (§13.1/§13.3).
///
/// `ctx`/`prospective` are the child instance's evaluation context and committed
/// state, against which an omitted member's default expression is evaluated (a
/// default referencing `$config` itself is a documented seam — CORE defaults are
/// literal or read only the child's own state).
pub(crate) fn resolve(
    schema: &ConfigSchema,
    supplied: &BTreeMap<String, Value>,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<BTreeMap<String, Value>, ConfigError> {
    let mut resolved = BTreeMap::new();
    for (name, value) in supplied {
        let Some(member_ty) = schema.member_type(name) else {
            return Err(ConfigError::UnknownMember(name.clone()));
        };
        resolved.insert(name.clone(), typecheck(name, member_ty, value)?);
    }
    for (name, _ty) in schema.members() {
        if resolved.contains_key(name) {
            continue;
        }
        let Some(default) = schema.default(name) else {
            return Err(ConfigError::MissingRequired(name.clone()));
        };
        let value = resolve_default(name, schema, default, &resolved, ctx, prospective)?;
        resolved.insert(name.clone(), value);
    }
    Ok(resolved)
}

/// Assemble the resolved values into the `$config` structural cell (§13.1): a
/// keyless struct row whose cells are the config members, so `$config` reads the
/// whole struct and `$config.member` reads one member.
#[must_use]
pub(crate) fn cell(resolved: &BTreeMap<String, Value>) -> Cell {
    let cells = resolved.iter().map(|(name, value)| (name.clone(), Cell::Scalar(value.clone())));
    Cell::Row(Box::new(Row::keyless(RowId::leaf(0), cells)))
}

/// Check a supplied value decodes to the declared member type (§13.3): re-encode
/// it to its canonical wire form and decode against the member's scalar type, so
/// a `bool` supplied where `text` is declared is rejected.
fn typecheck(name: &str, member_ty: &ExprType, value: &Value) -> Result<Value, ConfigError> {
    let Some(ty) = member_ty.as_scalar() else {
        return Err(ConfigError::TypeMismatch {
            member: name.to_owned(),
            detail: "member is not an installation value".to_owned(),
        });
    };
    ty.decode(&value.to_wire()).map_err(|error| ConfigError::TypeMismatch {
        member: name.to_owned(),
        detail: error.to_string(),
    })
}

/// Resolve an omitted member's default (§13.1: "defaults use the ordinary field
/// rules"). §4.2/§C.4: a literal default is decoded against the member type, and
/// an expression default is evaluated against the child state.
fn resolve_default(
    name: &str,
    schema: &ConfigSchema,
    default: &FieldDefault,
    resolved: &BTreeMap<String, Value>,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<Value, ConfigError> {
    match default {
        FieldDefault::Literal(LiteralDefault { wire, .. }) => {
            match schema.member_type(name).and_then(ExprType::as_scalar) {
                Some(ty) => ty.decode(wire).map_err(|error| ConfigError::DefaultFailed {
                    member: name.to_owned(),
                    detail: error.to_string(),
                }),
                None => Err(ConfigError::DefaultFailed {
                    member: name.to_owned(),
                    detail: "member is not an installation value".to_owned(),
                }),
            }
        }
        FieldDefault::Expr(source) => {
            eval_default(name, schema, &source.text, resolved, ctx, prospective)
        }
    }
}

/// Evaluate an omitted member's default expression against the child state, with
/// `.` bound to the config members resolved so far and `$config`'s type in scope
/// (§13.1: "defaults use the ordinary field rules").
fn eval_default(
    name: &str,
    schema: &ConfigSchema,
    text: &str,
    resolved: &BTreeMap<String, Value>,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<Value, ConfigError> {
    let config_row = ExprType::Row(schema.row_type().clone());
    let scope = RuntimeScope::new(config_row, ExprType::Row(ctx.schema.root_row_type()));
    let mut sources = SourceMap::new();
    let (typed, _src) = compile_expr(&mut sources, &scope, "config default", text)
        .map_err(|error| ConfigError::DefaultFailed { member: name.to_owned(), detail: error.to_string() })?;
    let current = cell(resolved);
    let result = ctx.eval(prospective, &typed, &current).map_err(|rejection| {
        ConfigError::DefaultFailed { member: name.to_owned(), detail: rejection.message().to_owned() }
    })?;
    let value = match result {
        Cell::Scalar(value) => value,
        Cell::Row(row) => row.key().clone(),
        Cell::Collection(_) => {
            return Err(ConfigError::DefaultFailed {
                member: name.to_owned(),
                detail: "default must evaluate to a scalar value".to_owned(),
            });
        }
    };
    match schema.member_type(name).and_then(ExprType::as_scalar) {
        Some(ty) => ty.decode(&value.to_wire()).map_err(|error| ConfigError::DefaultFailed {
            member: name.to_owned(),
            detail: error.to_string(),
        }),
        None => Ok(value),
    }
}
