//! The reserved `module` lifecycle namespace (§13.10).
//!
//! A host-privileged builtin mutation carries a module instance through its
//! lifecycle within a transition: `module.install(...)`, `module.update(...)`, or
//! `module.remove(...)`. This module owns the house-convention names — the reserved
//! namespace and the operation vocabulary — so the runtime interpreter and the
//! module host classify a lifecycle call against one authoritative source rather
//! than duplicating string literals. The privilege enforcement and the actual
//! mount/migration/removal live in the runtime; this is only the naming boundary.

/// The reserved namespace a lifecycle mutation addresses (`module.install(...)`).
/// A package cannot declare a collection or handle that shadows it in call
/// position, so a `module.<op>(...)` call is unambiguously a lifecycle builtin.
pub const LIFECYCLE_NAMESPACE: &str = "module";

/// How §13.16 spells an install / relocate: `.modules[@id] <- unpack(@package)`.
/// A diagnostic about [`LifecycleOp::InstallModule`] names the operator the author
/// actually wrote, exactly as the bare-call operators name theirs.
pub const MOVE_OPERATOR: &str = "<-";

/// A module-lifecycle operation a host-privileged builtin mutation performs
/// (§13.10, §13.16). The declarative spelling (`module.install(…)`) and the
/// §13.16 value-surface spelling (`pack(m)`, `update_module(m, u, …)`) name the
/// SAME operations: this enum is the single vocabulary both classify against, so
/// the two surfaces drive one runtime rather than two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleOp {
    /// Install a new instance from a blob-decoded package (§13.3).
    Install,
    /// Move a module VALUE into one entry of a module collection (§13.16 `<-`).
    ///
    /// Distinct from [`Self::Install`] on both halves of §13.16's "Install /
    /// override" rule: it addresses the package by module value rather than by a
    /// `blob` argument, and moving into an **occupied** slot replaces its occupant
    /// ("an occupant is dropped and uninstalled") where `module.install` refuses a
    /// duplicate name (§13.3 "unique within its module collection").
    InstallModule,
    /// Move a mounted module into an entry of a DIFFERENT module collection, by
    /// re-admitting it there (§13.16 `reinstall_module`). A plain move across
    /// collections is refused, because the destination declares its own §13.4
    /// parent surfaces, §13.5 peer set and §13.8 interface contracts; this operator
    /// is the explicit request to put the instance through that admission rather
    /// than rekey it underneath a boundary that was never checked.
    Reinstall,
    /// Update an existing instance to a blob-decoded package, walking the §20.1
    /// migration chain to the target version (§13.14).
    Update,
    /// Remove an existing instance (§13.12).
    Remove,
    /// Serialize an instance's axes into a `.liasse` blob (§13.16 `pack`).
    Pack,
    /// Apply a module VALUE onto a live instance (§13.16 `update_module`). Distinct
    /// from [`Self::Update`] because it addresses the instance by module value
    /// rather than by `(space, name)` and carries the `migrate` axis; its
    /// `migrate: model` half reaches the very same migration runtime.
    UpdateModule,
    /// Fork an instance's timeline back to a retained point (§13.16
    /// `rollback_module`).
    Rollback,
}

impl LifecycleOp {
    /// Classify a `module.<member>` call member as a lifecycle operation, or `None`
    /// when the member is not one of the reserved operation names. The caller has
    /// already matched the [`LIFECYCLE_NAMESPACE`].
    #[must_use]
    pub fn classify(member: &str) -> Option<Self> {
        match member {
            "install" => Some(Self::Install),
            "update" => Some(Self::Update),
            "remove" => Some(Self::Remove),
            _ => None,
        }
    }

    /// The reserved member name of this operation. The three declarative ops keep
    /// their `module.<member>` spelling; the two value-surface-only ops report the
    /// §13.16 operator name they are reached by, so a diagnostic always names the
    /// spelling the author actually wrote.
    #[must_use]
    pub fn member(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
            Self::Remove => "remove",
            Self::InstallModule => MOVE_OPERATOR,
            Self::Reinstall => ModuleOperator::Reinstall.name(),
            Self::Pack => ModuleOperator::Pack.name(),
            Self::UpdateModule => ModuleOperator::UpdateModule.name(),
            Self::Rollback => ModuleOperator::Rollback.name(),
        }
    }
}

/// A §13.16 module-value lifecycle operator, spelled as a bare call over module
/// values (`pack(m, { … })`) rather than through the reserved `module` namespace.
///
/// The operator names live here beside [`LifecycleOp`] for the same reason the
/// namespace does: the expression checker (which types the call), the interpreter
/// (which recognises it structurally before typing, to route it to the
/// host-privileged handle) and the module host (which performs it) all classify
/// against one authoritative source instead of three copies of three literals.
///
/// `unpack` is deliberately NOT here. It is the one half of the blob boundary that
/// needs no host authority — it wraps a blob descriptor in a deferred handle — so
/// it stays an ordinary `liasse-expr` builtin evaluated in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleOperator {
    /// `pack(m, { model?, data?, history? })` → `blob` (§13.16 blob boundary).
    Pack,
    /// `update_module(m, u, { migrate })` → the decoded package identity (§13.16).
    UpdateModule,
    /// `rollback_module(m, @point)` → the selected point identity (§13.16).
    Rollback,
    /// `reinstall_module(m)` → the module value, re-admitted at the destination
    /// (§13.16). Admitted ONLY as the source of a `<-` into a module-collection
    /// entry: it names the destination's admission, so it has no meaning where
    /// there is no destination.
    Reinstall,
}

impl ModuleOperator {
    /// Classify a bare call name as a §13.16 module-value operator.
    #[must_use]
    pub fn classify(name: &str) -> Option<Self> {
        match name {
            "pack" => Some(Self::Pack),
            "update_module" => Some(Self::UpdateModule),
            "rollback_module" => Some(Self::Rollback),
            "reinstall_module" => Some(Self::Reinstall),
            _ => None,
        }
    }

    /// The surface call name of this operator.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Pack => "pack",
            Self::UpdateModule => "update_module",
            Self::Rollback => "rollback_module",
            Self::Reinstall => "reinstall_module",
        }
    }

    /// The [`LifecycleOp`] this operator drives — the proof the value surface is a
    /// spelling of the §13.10 runtime and not a second mechanism.
    #[must_use]
    pub fn op(self) -> LifecycleOp {
        match self {
            Self::Pack => LifecycleOp::Pack,
            Self::UpdateModule => LifecycleOp::UpdateModule,
            Self::Rollback => LifecycleOp::Rollback,
            Self::Reinstall => LifecycleOp::Reinstall,
        }
    }
}

/// The argument-object member names the §13.16 operators accept. Named constants
/// rather than inline literals so the checker's diagnostics, the interpreter's
/// argument marshalling and the host's argument reading cannot drift apart.
pub mod arg {
    /// `pack`'s definition axis — addressed by version.
    pub const MODEL: &str = "model";
    /// `pack`'s state axis — addressed by a point in time.
    pub const DATA: &str = "data";
    /// `pack`'s history axis — addressed by a time range.
    pub const HISTORY: &str = "history";
    /// `update_module`'s migration axis (`model` or `model+data`).
    pub const MIGRATE: &str = "migrate";
    /// The module operand every §13.16 operator takes first.
    pub const MODULE: &str = "module";
    /// The module-collection entry a slot-addressed lifecycle call writes
    /// (`module.install({ at: .modules[@name], … })`). Its value is a path the
    /// interpreter resolves to an ordinary row address, not a coordinate string.
    pub const AT: &str = "at";
    /// `update_module`'s second module operand — the definition applied onto the
    /// live instance.
    pub const ONTO: &str = "onto";
    /// `rollback_module`'s retained-point coordinate.
    pub const POINT: &str = "point";
}

/// The `migrate` axis of `update_module` (§13.16): which of a module's axes the
/// update carries across.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrateAxis {
    /// `migrate: model` — migrate the schema to `u`'s definition and carry `m`'s
    /// current data forward (§20.1). The default.
    Model,
    /// `migrate: model+data` — additionally forward `u`'s history and data,
    /// reconciled by lineage (fast-forward, or a loud divergence).
    ModelAndData,
}

impl MigrateAxis {
    /// Parse the `migrate` member's spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "model" => Some(Self::Model),
            "model+data" => Some(Self::ModelAndData),
            _ => None,
        }
    }

    /// The spelling of this axis.
    #[must_use]
    pub fn spelling(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::ModelAndData => "model+data",
        }
    }

    /// Every accepted spelling, for a diagnostic that lists the alternatives.
    pub const SPELLINGS: [&'static str; 2] = ["model", "model+data"];
}
