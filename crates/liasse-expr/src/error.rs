//! Evaluation errors.
//!
//! Type errors are reported as [`Diagnostics`](liasse_diag::Diagnostics) while
//! checking; past that boundary a [`TypedExpr`](crate::TypedExpr) is proof the
//! expression is well-typed, so evaluation fails only on conditions that a
//! static type cannot exclude (§8.3): a zero divisor read from state, a
//! selector context that requires exactly one row but gets zero or several, a
//! ref that resolves to no target row, a value error from a checked conversion.
//!
//! Every variant is a *typed* failure — never a panic. The workspace lints
//! forbid `unwrap`/`panic`/overflow, so the evaluator routes every fallible
//! step (integer parse, decimal scale, arity) through [`EvalError`].

use liasse_value::ValueError;

/// A condition that fails a well-typed evaluation at run time (§8.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// A `/` or `%` with a zero divisor read from state (SPEC-ISSUES item 3:
    /// division by zero is unpinned; this crate surfaces it as a typed error
    /// rather than producing a value or panicking).
    DivisionByZero,

    /// A context requiring exactly one row occurrence (a row-mutation receiver,
    /// `$actor`, scalar-row conversion, a scalar-key dereference) received a
    /// number of occurrences that is not one (§6.3).
    Cardinality {
        /// What the context is.
        context: &'static str,
        /// How many occurrences were found.
        found: usize,
    },

    /// A binding, parameter, structural name, or import that the checker
    /// resolved statically was absent from the environment at run time. This is
    /// an environment/host contract breach, not authoring error.
    UnboundName {
        /// The kind of binding (`parameter`, `structural`, `import`, `binding`).
        kind: &'static str,
        /// The name.
        name: String,
    },

    /// A cell held a shape the checker's type said it would not (e.g. a scalar
    /// where a collection was typed). An environment contract breach.
    ShapeMismatch {
        /// What the evaluator expected.
        expected: &'static str,
    },

    /// A checked conversion or value construction failed at run time.
    Value(ValueError),

    /// A decimal division or aggregate produced a scale beyond the supported
    /// bound (mirrors [`ValueError::DecimalScaleOutOfRange`] guarding the
    /// canonical-text encoder).
    DecimalScale,

    /// A `.$between(a, b)` selector received an empty or reversed range
    /// (`b <= a`); §14.1 rejects evaluation of such a query.
    EmptyTemporalRange,

    /// A temporal selector (`.$at`/`.$between`/`.$all`) was evaluated against an
    /// environment that supplies no temporal index (§14.1). An environment
    /// contract breach: a bucketed read must run against a bucket-aware
    /// environment.
    NoTemporalIndex,

    /// A keyring selector (`.$current`/`.$accepted`/`.$public`/`.$versions`) was
    /// evaluated against an environment that owns no keyring (§17.2). An
    /// environment contract breach: a keyring read must run against a
    /// keyring-aware environment.
    NoKeyringIndex,
}

impl EvalError {
    /// A one-line explanation for logs and test assertions.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::DivisionByZero => "division by zero".to_owned(),
            Self::Cardinality { context, found } => {
                format!("{context} requires exactly one row, found {found}")
            }
            Self::UnboundName { kind, name } => format!("unbound {kind} `{name}`"),
            Self::ShapeMismatch { expected } => {
                format!("environment supplied a value that is not {expected}")
            }
            Self::Value(err) => err.to_string(),
            Self::DecimalScale => "decimal result scale exceeds the supported bound".to_owned(),
            Self::EmptyTemporalRange => {
                "`.$between(a, b)` requires `b > a`; the range is empty or reversed".to_owned()
            }
            Self::NoTemporalIndex => {
                "a temporal selector needs an environment with a temporal index".to_owned()
            }
            Self::NoKeyringIndex => {
                "a keyring selector needs an environment that owns the keyring".to_owned()
            }
        }
    }
}

impl From<ValueError> for EvalError {
    fn from(err: ValueError) -> Self {
        Self::Value(err)
    }
}
