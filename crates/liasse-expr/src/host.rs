//! Host-namespace call typing (§16.2/§16.3/§16.5): the pinned signature a call
//! site is checked against, and the position policy that decides which effect
//! classes and which namespace origins may run where.
//!
//! liasse-expr does not depend on liasse-host — a namespace descriptor is a
//! host-layer artefact, and the checker only needs its *shape*: positional
//! parameter types, a result type, an effect class, and its origin (a built-in
//! namespace, or an application namespace registered through `$requires`). The
//! runtime, which owns both crates, translates a resolved `liasse_host`
//! descriptor into a [`HostOp`] and exposes it to the checker through
//! [`Scope::namespace_op`](crate::Scope).

use liasse_value::Type;

/// The §16.3 effect class a host-namespace function declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostEffect {
    /// Same logical inputs produce the same output. MAY run in views, checks,
    /// defaults, and replay (§16.3).
    Pure,
    /// Validates untrusted input against declared keys/configuration and returns
    /// a typed proof or diagnostic. A verifier runs inside the mutation that
    /// admits its request (§16.3, §16.5), never in a read/replay position.
    Verifier,
    /// May use randomness, clocks, or provider operations; one successful result
    /// is fixed for the admitted operation. A generated function produces its
    /// recorded result inside the mutation that commits it (§16.3, §16.5).
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

/// Whether a host-namespace function is a built-in the engine links (Core) or an
/// application namespace registered through `$requires` (Registered) (§16.5).
///
/// Under §16.5 only a built-in (Core) function may run in a database-evaluated
/// position; a call to a `$requires`-registered namespace is legal only inside a
/// mutation program body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOrigin {
    /// A built-in namespace the engine links and can evaluate inside its storage
    /// engine: the §6.5 core namespaces (`string`/`time`/`convert`/`hex`/
    /// `base64`/`sha`) and the §20 core codecs (`hex`/`base64`/`string`-bytes).
    Core,
    /// An application namespace registered through `$requires` (§16.2). Its one
    /// legal call position is a mutation program body (§16.5).
    Registered,
}

impl HostOrigin {
    /// The spelling used in diagnostics.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Core => "a built-in",
            Self::Registered => "an app-registered",
        }
    }
}

/// The sub-position a database-evaluated expression sits in (§16.5). It carries no
/// policy of its own — every variant is restricted identically (built-in
/// namespaces only, pure effect only) — and exists only to name the position in
/// the load-time diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbReadPosition {
    /// A `$view` `[:x | …]` filter (§7.1/§10.1).
    ViewFilter,
    /// A `$view` projection output or synthetic `$key` grouping (§7.1/§7.2).
    ViewProjection,
    /// A `$sort` key or `$skip`/`$limit` bound (§7.3).
    SortKey,
    /// A `$recursive` `$where`/`$except`/`$through` coverage predicate (§10.5).
    Coverage,
    /// A computed value (§5.2).
    Computed,
    /// A field, struct, or row `$check` (§5.10/§8.8).
    Check,
    /// A `$normalize` (§8.8).
    Normalize,
    /// An authenticator `$verify` (§11.3). Native token/keyring verification is
    /// runtime-dispatched, not a host op, so only an app-namespace call is caught.
    Verify,
    /// An authenticator `$actor`/`$session` selector (§11.3).
    ActorSession,
    /// A role `$members` selection (§10.3): the membership view that decides who
    /// holds the role. Database-evaluated like any other view.
    RoleMembers,
    /// A bucket `$from`/`$until`/`$repeat`/`$order` bound (§14).
    BucketBound,
    /// A meter pool/`$quantity`/`$amount`/`$time`/`$eligible` expression (§15).
    MeterSource,
    /// A blob placement `$in`/`$serve` store view (§18.4).
    Placement,
    /// A migration field transform `$as`/`$back` (§20.1/§20.2).
    MigrationTransform,
    /// A surface `$mut` receiver selector outside the named mutation's body
    /// (§10.1) — a read/selection, not the framework mutation body.
    Receiver,
}

impl DbReadPosition {
    /// The spelling used in diagnostics.
    #[must_use]
    const fn describe(self) -> &'static str {
        match self {
            Self::ViewFilter => "a view filter",
            Self::ViewProjection => "a view projection",
            Self::SortKey => "a `$sort` key",
            Self::Coverage => "a `$recursive` `$where`/`$except`",
            Self::Computed => "a computed value",
            Self::Check => "a `$check`",
            Self::Normalize => "a `$normalize`",
            Self::Verify => "an auth `$verify`",
            Self::ActorSession => "an auth `$actor`/`$session`",
            Self::RoleMembers => "a role `$members` view",
            Self::BucketBound => "a bucket bound",
            Self::MeterSource => "a meter expression",
            Self::Placement => "a blob placement view",
            Self::MigrationTransform => "a migration `$as`/`$back` transform",
            Self::Receiver => "a `$mut` receiver selector",
        }
    }
}

/// Which checking position a host-namespace call sits in — the axis §16.3/§8.8
/// (effect class) and §16.5 (namespace origin) use to decide whether a call is
/// admissible there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPosition {
    /// A database-evaluated position: everything but a mutation body (§16.5). Only
    /// built-in namespaces (Core) and pure functions may run. The kind names the
    /// sub-position for the diagnostic.
    DbRead(DbReadPosition),
    /// A field default (§5.1): still built-in-only for *namespace* calls (Core +
    /// pure), but the language generated functions (`uuid()`, `now()`) stay legal
    /// (§8.8) — they are typed as language calls and never reach
    /// [`check_host_call`](crate::check) at all.
    Default,
    /// The mutation program body — the transaction (§8, §16.5). The only position
    /// where an app-registered namespace may be called, with any declared effect
    /// class (pure, verifier, generated — §16.3).
    Mutation,
}

impl HostPosition {
    /// Whether a function of `effect` may run in this position (§16.3, §8.8): a
    /// database-evaluated position and a field default admit only pure functions;
    /// a mutation body admits any effect class.
    #[must_use]
    pub const fn permits_effect(self, effect: HostEffect) -> bool {
        match self {
            Self::DbRead(_) | Self::Default => matches!(effect, HostEffect::Pure),
            Self::Mutation => true,
        }
    }

    /// Whether a call of `origin` may run in this position (§16.5): a
    /// database-evaluated position and a field default admit only built-in (Core)
    /// namespaces; a mutation body admits an app-registered namespace too.
    #[must_use]
    pub const fn permits_origin(self, origin: HostOrigin) -> bool {
        match self {
            Self::DbRead(_) | Self::Default => matches!(origin, HostOrigin::Core),
            Self::Mutation => true,
        }
    }

    /// The spelling of this position in the §16.3 effect-class diagnostic.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::DbRead(kind) => kind.describe(),
            Self::Default => "a field default",
            Self::Mutation => "a mutation program",
        }
    }
}

/// A resolved host-namespace function's pinned signature (§16.2): its positional
/// parameter types, result type, effect class, and origin — the descriptor entry a
/// call site type-checks against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostOp {
    params: Vec<Type>,
    result: Type,
    effect: HostEffect,
    origin: HostOrigin,
}

impl HostOp {
    /// Assemble a resolved host op from its signature, effect class, and origin.
    #[must_use]
    pub fn new(
        params: impl IntoIterator<Item = Type>,
        result: Type,
        effect: HostEffect,
        origin: HostOrigin,
    ) -> Self {
        Self {
            params: params.into_iter().collect(),
            result,
            effect,
            origin,
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

    /// The namespace origin (§16.5): a built-in the engine links, or an
    /// application namespace registered through `$requires`.
    #[must_use]
    pub const fn origin(&self) -> HostOrigin {
        self.origin
    }
}
