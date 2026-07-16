//! The conformance guard: a checked-invocation wrapper that does not trust a
//! registered namespace to honour the typed contract it declared (§2.1/§16.2
//! assume conformance; the runtime must guard against a component that does
//! not — SPEC-ISSUES items 15 and 16).
//!
//! [`ConformanceGuard`] wraps one namespace and, on each invocation, validates
//! the returned value against the function's declared result type and — for a
//! `pure` function — against the values it returned for the same arguments
//! before. A violation is reported as a typed [`ConformanceViolation`]; the
//! guard makes no policy decision (reject, quarantine, or degrade) — that is the
//! runtime's, per the two unpinned items.

use std::collections::BTreeMap;

use liasse_value::{Type, Value, ValueError};

use crate::descriptor::EffectClass;
use crate::namespace::{HostNamespace, InvocationFailure};

/// The checked-invocation policy over a namespace. It owns only the drift-
/// detection memo (an owned `BTreeMap`, no interior mutability); the namespace
/// is passed to each call rather than held, so the caller may `&mut`-advance a
/// misbehaving component between invocations (e.g. across a replay).
#[derive(Default)]
pub struct ConformanceGuard {
    memo: BTreeMap<(String, Vec<Value>), Value>,
}

impl ConformanceGuard {
    /// A fresh guard with an empty drift memo.
    #[must_use]
    pub fn new() -> Self {
        Self {
            memo: BTreeMap::new(),
        }
    }

    /// Invoke `function` on `namespace` with typed `args`, checking the outcome
    /// against the namespace's declared contract.
    ///
    /// A contract-honouring failure (a verifier diagnostic, an unavailable
    /// dependency) is surfaced verbatim as [`GuardError::Invocation`]. A
    /// success is checked two ways:
    ///
    /// 1. **Type conformance (item 15).** The returned value is re-decoded
    ///    against the declared result type. A value that does not conform is a
    ///    [`ConformanceViolation::OffContractType`] — the component returned an
    ///    off-contract type.
    /// 2. **Effect conformance (items 15/16).** For a function declared `pure`,
    ///    a later call with equal arguments that returns a different value is a
    ///    [`ConformanceViolation::PureDrift`] — the component lied about its
    ///    effect class.
    pub fn invoke(
        &mut self,
        namespace: &dyn HostNamespace,
        function: &str,
        args: &[Value],
    ) -> Result<Value, GuardError> {
        let descriptor = namespace.descriptor();
        let declared = descriptor.function(function).ok_or_else(|| {
            GuardError::Violation(Box::new(ConformanceViolation::UndeclaredFunction(
                function.to_owned(),
            )))
        })?;
        let result_type = declared.signature().result().clone();
        let effect = declared.effect();

        let returned = namespace
            .invoke(function, args)
            .map_err(GuardError::Invocation)?;

        // (1) The value must conform to the declared result type. A successful
        // decode against that type is proof of conformance (parse-don't-validate).
        if let Err(reason) = result_type.decode(&returned.to_wire()) {
            return Err(GuardError::Violation(Box::new(
                ConformanceViolation::OffContractType {
                    function: function.to_owned(),
                    declared: result_type,
                    returned: returned.to_canonical_json_string(),
                    reason,
                },
            )));
        }

        // (2) A pure function must be a stable map from arguments to result.
        if effect == EffectClass::Pure {
            let key = (function.to_owned(), args.to_vec());
            if let Some(previous) = self.memo.get(&key) {
                if previous != &returned {
                    return Err(GuardError::Violation(Box::new(
                        ConformanceViolation::PureDrift {
                            function: function.to_owned(),
                            first: previous.to_canonical_json_string(),
                            second: returned.to_canonical_json_string(),
                        },
                    )));
                }
            } else {
                self.memo.insert(key, returned.clone());
            }
        }

        Ok(returned)
    }
}

/// The outcome of a guarded invocation that is not a plain success.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuardError {
    /// The namespace reported a contract-honouring failure (§16.3). Not a
    /// conformance problem — a verifier rejecting bad input is expected.
    #[error(transparent)]
    Invocation(InvocationFailure),
    /// The namespace violated the typed contract it registered. Boxed because a
    /// violation carries the declared type and the offending value's wire form,
    /// which would otherwise make the common `Ok` path pay for the rare error.
    #[error(transparent)]
    Violation(Box<ConformanceViolation>),
}

/// A way a registered namespace broke the typed contract it declared. The
/// runtime decides policy on these (SPEC-ISSUES items 15/16 record what is
/// unpinned); the guard only detects and names them.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConformanceViolation {
    /// The guard was asked to invoke a function the descriptor does not
    /// declare, so there is no signature to check against.
    #[error("namespace does not declare function `{0}`")]
    UndeclaredFunction(String),

    /// The function returned a value that does not conform to its declared
    /// result type (SPEC-ISSUES item 15: a component returning an off-contract
    /// type).
    #[error("function `{function}` returned `{returned}`, which is not a `{}`: {reason}", declared.name())]
    OffContractType {
        /// The offending function.
        function: String,
        /// Its declared result type.
        declared: Type,
        /// The canonical wire form of the non-conforming value.
        returned: String,
        /// Why the value failed to decode against the declared type.
        reason: ValueError,
    },

    /// A `pure` function returned different values for equal arguments
    /// (SPEC-ISSUES items 15/16: a component lying about its effect class; the
    /// spec permits recomputing pure functions during replay, which such drift
    /// would make unsound).
    #[error("pure function `{function}` returned `{first}` then `{second}` for equal arguments")]
    PureDrift {
        /// The offending function.
        function: String,
        /// The first result observed.
        first: String,
        /// The divergent later result.
        second: String,
    },
}
