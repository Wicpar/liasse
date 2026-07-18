//! Structural type conformance: does a runtime [`Value`]'s *variant* satisfy a
//! declared [`Type`]?
//!
//! The conformance guard cannot decide this by round-tripping the value through
//! its canonical wire form: several distinct types share a wire spelling (an
//! `int`, a numeric-looking `text`, a `uuid`, a `date`, a `decimal`, and an
//! `enum` label all canonicalise to a JSON string), so a wire re-decode waves a
//! wrong variant through whenever its spelling happens to be a legal member of
//! the declared type. §16.2 pins each function's *typed* signature, so the
//! promise is about the value's type — its [`Value`] variant — not merely a
//! string that could be parsed as one. This module compares variant against
//! declared type directly and recurses into composites, so a `Value::Text("42")`
//! returned where `int` is declared is caught even though `"42"` decodes as an
//! int.

use liasse_value::{RefKey, Type, Value};

/// Compare a runtime value's structure against a declared type.
///
/// Implemented on [`Type`] (an extension trait, since the type lives in a
/// sibling crate) so the guard can write `declared.conforms(&returned)`.
pub trait TypeConformance {
    /// `Ok(())` when `value`'s variant (recursively, for composites) satisfies
    /// this declared type; otherwise the first structural discrepancy found.
    fn conforms(&self, value: &Value) -> Result<(), TypeMismatch>;
}

impl TypeConformance for Type {
    fn conforms(&self, value: &Value) -> Result<(), TypeMismatch> {
        match (self, value) {
            (Type::Text, Value::Text(_))
            | (Type::Bool, Value::Bool(_))
            | (Type::Int, Value::Int(_))
            | (Type::Decimal, Value::Decimal(_))
            | (Type::Bytes, Value::Bytes(_))
            | (Type::Uuid, Value::Uuid(_))
            | (Type::Date, Value::Date(_))
            | (Type::Timestamp(_), Value::Timestamp(_))
            | (Type::Duration, Value::Duration(_))
            | (Type::Period, Value::Period(_))
            | (Type::Json, Value::Json(_))
            | (Type::Blob, Value::Blob(_)) => Ok(()),

            (Type::Enum(declared), Value::Enum(actual)) => {
                if declared.labels().iter().any(|label| label == actual.label()) {
                    Ok(())
                } else {
                    Err(TypeMismatch::EnumLabel {
                        label: actual.label().to_owned(),
                        allowed: declared.labels().to_vec(),
                    })
                }
            }

            // A.1: `none` is the absent `optional<T>`; a present value must
            // conform to the inner type.
            (Type::Optional(_), Value::None) => Ok(()),
            (Type::Optional(inner), present) => inner
                .conforms(present)
                .map_err(|reason| TypeMismatch::nested("optional value", reason)),

            (Type::Set(inner), Value::Set(members)) => {
                for member in members {
                    inner
                        .conforms(member)
                        .map_err(|reason| TypeMismatch::nested("set element", reason))?;
                }
                Ok(())
            }

            (Type::Map(key_type, value_type), Value::Map(entries)) => {
                for (key, val) in entries {
                    key_type
                        .conforms(key)
                        .map_err(|reason| TypeMismatch::nested("map key", reason))?;
                    value_type
                        .conforms(val)
                        .map_err(|reason| TypeMismatch::nested("map value", reason))?;
                }
                Ok(())
            }

            (Type::Ref(target), Value::Ref(reference)) => target.conforms(reference.key()),

            (Type::Struct(declared), Value::Struct(actual)) => {
                for (name, _) in actual.fields() {
                    if declared.field(name.as_str()).is_none() {
                        return Err(TypeMismatch::UnexpectedField(name.as_str().to_owned()));
                    }
                }
                for (name, field_type) in declared.fields() {
                    match actual.get(name) {
                        Some(member) => field_type
                            .conforms(member)
                            .map_err(|reason| TypeMismatch::nested_owned(name.clone(), reason))?,
                        // A.1: an absent optional field carries `none`; a
                        // required field is missing.
                        None if matches!(field_type, Type::Optional(_)) => {}
                        None => return Err(TypeMismatch::MissingField(name.clone())),
                    }
                }
                Ok(())
            }

            (Type::Composite(components), Value::Composite(values)) => {
                if components.len() != values.len() {
                    return Err(TypeMismatch::RefArity {
                        expected: components.len(),
                        found: values.len(),
                    });
                }
                for ((_, component_type), value) in components.iter().zip(values) {
                    component_type
                        .conforms(value)
                        .map_err(|reason| TypeMismatch::nested("composite key component", reason))?;
                }
                Ok(())
            }

            (declared, actual) => Err(TypeMismatch::Variant {
                expected: declared.name(),
                found: actual.variant_name(),
            }),
        }
    }
}

/// A declared `ref<T>` target key type checked against a runtime ref key (A.9).
trait RefTargetConformance {
    fn conforms(&self, key: &RefKey) -> Result<(), TypeMismatch>;
}

impl RefTargetConformance for liasse_value::RefTarget {
    fn conforms(&self, key: &RefKey) -> Result<(), TypeMismatch> {
        match (self, key) {
            (liasse_value::RefTarget::Scalar(inner), RefKey::Scalar(value)) => inner
                .conforms(value)
                .map_err(|reason| TypeMismatch::nested("ref key", reason)),
            (liasse_value::RefTarget::Composite(types), RefKey::Composite(values)) => {
                if types.len() != values.len() {
                    return Err(TypeMismatch::RefArity {
                        expected: types.len(),
                        found: values.len(),
                    });
                }
                for ((_, component_type), component) in types.iter().zip(values) {
                    component_type
                        .conforms(component)
                        .map_err(|reason| TypeMismatch::nested("ref key component", reason))?;
                }
                Ok(())
            }
            (liasse_value::RefTarget::Scalar(_), RefKey::Composite(_)) => Err(TypeMismatch::Variant {
                expected: "scalar ref key",
                found: "composite ref key",
            }),
            (liasse_value::RefTarget::Composite(_), RefKey::Scalar(_)) => {
                Err(TypeMismatch::Variant {
                    expected: "composite ref key",
                    found: "scalar ref key",
                })
            }
        }
    }
}

/// Name a runtime value's variant for a diagnostic, mirroring [`Type::name`].
trait VariantName {
    fn variant_name(&self) -> &'static str;
}

impl VariantName for Value {
    fn variant_name(&self) -> &'static str {
        match self {
            Value::Text(_) => "text",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Decimal(_) => "decimal",
            Value::Bytes(_) => "bytes",
            Value::Uuid(_) => "uuid",
            Value::Date(_) => "date",
            Value::Timestamp(_) => "timestamp",
            Value::Duration(_) => "duration",
            Value::Period(_) => "period",
            Value::Json(_) => "json",
            Value::Blob(_) => "blob",
            Value::Enum(_) => "enum",
            Value::Ref(_) => "ref",
            Value::Struct(_) => "struct",
            Value::Composite(_) => "composite key",
            Value::Set(_) => "set",
            Value::Map(_) => "map",
            Value::None => "none",
        }
    }
}

/// Why a returned value's structure does not satisfy a declared type. Distinct
/// from `liasse_value::ValueError` (a *wire-decode* diagnostic): this compares
/// an already-decoded value's variant, catching a wrong type whose wire
/// spelling would round-trip cleanly.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TypeMismatch {
    /// The value is the wrong variant for the declared type outright.
    #[error("expected a `{expected}` value, found a `{found}`")]
    Variant {
        /// The declared type's name.
        expected: &'static str,
        /// The offending value's variant name.
        found: &'static str,
    },

    /// An `enum` value carries a label the declared enum does not declare.
    #[error("`enum` label `{label}` is not one of the declared labels {allowed:?}")]
    EnumLabel {
        /// The offending label.
        label: String,
        /// The declared labels, in declaration order.
        allowed: Vec<String>,
    },

    /// A composite `ref` key has the wrong number of components.
    #[error("composite ref key has {found} component(s) but the target declares {expected}")]
    RefArity {
        /// The declared component count.
        expected: usize,
        /// The count the value carried.
        found: usize,
    },

    /// A struct is missing a required (non-optional) declared field.
    #[error("missing required struct field `{0}`")]
    MissingField(String),

    /// A struct carries a member the declared struct type does not declare.
    #[error("unexpected struct member `{0}`")]
    UnexpectedField(String),

    /// A nested position (optional value, set element, map key/value, struct
    /// field, ref component) did not conform.
    #[error("in {location}: {reason}")]
    Nested {
        /// Where inside the composite the discrepancy was found.
        location: String,
        /// The nested discrepancy.
        reason: Box<TypeMismatch>,
    },
}

impl TypeMismatch {
    fn nested(location: &'static str, reason: TypeMismatch) -> Self {
        Self::Nested {
            location: location.to_owned(),
            reason: Box::new(reason),
        }
    }

    fn nested_owned(location: String, reason: TypeMismatch) -> Self {
        Self::Nested {
            location: format!("struct field `{location}`"),
            reason: Box::new(reason),
        }
    }
}
