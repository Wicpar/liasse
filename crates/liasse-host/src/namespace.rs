//! The [`HostNamespace`] contract: a typed function-and-value namespace
//! implemented in Rust and registered in a Liasse context (§16).
//!
//! The trait is object-safe and synchronous: a runtime holds heterogeneous
//! registrations behind `&dyn HostNamespace`, receives typed [`Value`]
//! arguments, and gets back either a typed result or a typed [`InvocationFailure`].
//! A namespace has no interior mutability of its own; a pure or verifier call
//! is a read (`&self`).

use liasse_value::{Type, Value};

use crate::descriptor::NamespaceDescriptor;

/// A registered host namespace (§16).
///
/// Object-safe by construction: no generic methods, no `Self` returns. The
/// runtime resolves a `$requires` entry to one of these and invokes its
/// functions with typed arguments.
pub trait HostNamespace: Send + Sync {
    /// The pinned load-time descriptor (§16.2): named types, typed function
    /// signatures, effect classes, and the semantic interface hash. The
    /// runtime resolves and pins this; the model type-checks call sites
    /// against it.
    fn descriptor(&self) -> &NamespaceDescriptor;

    /// Invoke a declared function with typed arguments, yielding a typed result
    /// or a typed failure.
    ///
    /// A `pure` function is a mathematical map from `args` to result; a
    /// `verifier` returns a proof value or an [`InvocationFailure::Verification`]
    /// diagnostic; a `generated` function may consult randomness/clocks/providers
    /// and fixes one result for the admitted operation. The runtime — not the
    /// namespace — enforces *where* each effect class may run (§16.3); this
    /// method simply performs the operation.
    fn invoke(&self, function: &str, args: &[Value]) -> Result<Value, InvocationFailure>;
}

/// A typed failure returned by [`HostNamespace::invoke`].
///
/// This is the *contract-honouring* failure surface: a well-behaved namespace
/// reports these. A namespace that instead returns an off-contract *value* (a
/// wrong type, or divergent pure results) is a nonconforming component; that is
/// caught by the checked-invocation guard, not represented here (SPEC-ISSUES
/// items 15/16).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvocationFailure {
    /// The namespace does not declare a function by this name.
    #[error("namespace has no function `{0}`")]
    UnknownFunction(String),

    /// The call supplied the wrong number of arguments for the signature.
    #[error("function `{function}` expects {expected} argument(s), got {found}")]
    Arity {
        /// The called function.
        function: String,
        /// The declared parameter count.
        expected: usize,
        /// The supplied argument count.
        found: usize,
    },

    /// An argument's type did not match the pinned signature at `position`.
    #[error("function `{function}` argument {position} expects `{}`, got `{found}`", expected.name())]
    Argument {
        /// The called function.
        function: String,
        /// Zero-based argument position.
        position: usize,
        /// The declared parameter type.
        expected: Type,
        /// The name of the type actually supplied.
        found: &'static str,
    },

    /// A verifier rejected its untrusted input: the credential/proof did not
    /// validate against the declared keys/configuration (§16.3). Authentication
    /// fails. The detail is a sanitized, call-local explanation (§23.8).
    #[error("verification failed: {detail}")]
    Verification {
        /// A sanitized explanation, safe to surface.
        detail: String,
    },

    /// The namespace could not perform the operation because a resource it
    /// depends on (a bound provider, a keyring's active version) is unavailable
    /// (§17.9). No application effect is committed.
    #[error("namespace operation unavailable: {detail}")]
    Unavailable {
        /// What was unavailable.
        detail: String,
    },
}
