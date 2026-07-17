//! Host-namespace call typing (§16.2/§16.3): the pinned signature a call site is
//! checked against, and the position policy that decides which effect classes
//! may run where.
//!
//! liasse-expr does not depend on liasse-host — a namespace descriptor is a
//! host-layer artefact, and the checker only needs its *shape*: positional
//! parameter types, a result type, and an effect class. The runtime, which owns
//! both crates, translates a resolved `liasse_host` descriptor into a [`HostOp`]
//! and exposes it to the checker through [`Scope::namespace_op`](crate::Scope).

use liasse_value::Type;

/// The §16.3 effect class a host-namespace function declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostEffect {
    /// Same logical inputs produce the same output. MAY run in views, checks,
    /// defaults, and replay (§16.3).
    Pure,
    /// Validates untrusted input against declared keys/configuration and returns
    /// a typed proof or diagnostic. Runs during external request admission
    /// (an auth `$verify`), never in a read/replay position (§16.3).
    Verifier,
    /// May use randomness, clocks, or provider operations; one successful result
    /// is fixed for the admitted operation. Runs in mutation/write-time positions
    /// (a field default, a mutation value), never in a view/check (§16.3, §8.8).
    Generated,
}

impl HostEffect {
    /// The spelling used in diagnostics.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Pure => "pure",
            Self::Verifier => "verifier",
            Self::Generated => "generated",
        }
    }
}

/// Which checking position a host-namespace call sits in — the axis §16.3/§8.8
/// use to decide whether an effect class is admissible there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPosition {
    /// A read/replay position: a view, computed value, `$normalize`, `$check`,
    /// bucket bound, or meter expression. Only pure functions may run (§16.3:
    /// "Pure functions MAY run during views, checks, and replay").
    Pure,
    /// A write position: a field default or a mutation-program value. Pure and
    /// generated functions may run (§8.8: "defaults MAY use generated
    /// functions"; §16.3: generated functions run write-time).
    Write,
    /// An admission-verifier position: an auth `$verify`. Pure and verifier
    /// functions may run (§16.3: "Verifiers run during external request
    /// admission").
    Admission,
}

impl HostPosition {
    /// Whether a function of `effect` may run in this position (§16.3, §8.8).
    #[must_use]
    pub const fn permits(self, effect: HostEffect) -> bool {
        match (self, effect) {
            // A pure function is a mathematical map; it may run in any position.
            (_, HostEffect::Pure) => true,
            (Self::Write, HostEffect::Generated) => true,
            (Self::Admission, HostEffect::Verifier) => true,
            _ => false,
        }
    }

    /// The spelling used in diagnostics.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Pure => "a view/check position",
            Self::Write => "a write position",
            Self::Admission => "an admission position",
        }
    }
}

/// A resolved host-namespace function's pinned signature (§16.2): its positional
/// parameter types, result type, and effect class — the descriptor entry a call
/// site type-checks against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostOp {
    params: Vec<Type>,
    result: Type,
    effect: HostEffect,
}

impl HostOp {
    /// Assemble a resolved host op from its signature and effect class.
    #[must_use]
    pub fn new(params: impl IntoIterator<Item = Type>, result: Type, effect: HostEffect) -> Self {
        Self {
            params: params.into_iter().collect(),
            result,
            effect,
        }
    }

    /// The positional parameter types, in call order.
    #[must_use]
    pub fn params(&self) -> &[Type] {
        &self.params
    }

    /// The result type.
    #[must_use]
    pub fn result(&self) -> &Type {
        &self.result
    }

    /// The declared effect class (§16.3).
    #[must_use]
    pub const fn effect(&self) -> HostEffect {
        self.effect
    }
}
