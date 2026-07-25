//! Module composition runtime (§13).
//!
//! A [`ModuleHost`] owns a root [`Engine`](crate::Engine) and the child instances
//! mounted in its **module collections** — ordinary maps of `module` values
//! (`{ $key: text, $value: module }`), one instance per entry. An instance is
//! addressed by the [`RowAddress`](liasse_store::RowAddress) of its entry, which
//! is the same address every other row has, so containment is whatever the
//! containing collections give and there is no module-space coordinate system.
//! Each mounted instance is its own independently loaded
//! [`Engine`](crate::Engine) over a store the host's
//! [`StoreFactory`](liasse_store::StoreFactory) mints (§13.1: "each installed
//! instance owns its private model, data, history, configuration"), so isolation
//! is structural — nothing but the declared boundary crosses between instances.
//!
//! # What this layer does (CORE this increment)
//!
//! - **Install + mount** ([`ModuleHost::install`]): admits an [`InstallRequest`]
//!   into one entry of a module collection, recording its `$config`/`$use`/`$deps`
//!   boundary bindings ([`AdmittedBindings`], §13.3 `$resolved`) on the new
//!   instance; the same package installed at two entries yields isolated instances
//!   (§13.2).
//! - **Interface-addressed read** ([`ModuleHost::interface_read`]): evaluates a
//!   child's `$expose`d interface `$view` through the boundary (§13.8) — only
//!   projected fields cross, so a private field is unreachable (isolation). Reading
//!   the interface across a whole collection needs no separate operation: the
//!   entries materialize as ordinary rows, so `.modules::iface` is the §6.4 nested
//!   traversal.
//! - **Lifecycle** ([`ModuleHost::disable`]/[`ModuleHost::enable`]/
//!   [`ModuleHost::uninstall`], plus [`ModuleHost::rename`]/[`ModuleHost::update`]):
//!   disable removes a child's active boundary occurrences (so it contributes no
//!   entry to the materialized collection) while retaining its private state and
//!   history; enable restores them (§13.3, §13.12).
//! - **Seed three-way merge** ([`SeedMerge`], §13.13) as a pure rule.
//!
//! # Documented seams (next rounds)
//!
//! - Installation `$data` overlay onto the child genesis (§13.3), and wiring the
//!   [`SeedMerge`] rule into the update seed pass (§13.13).
//! - Peer/parent resolution against the sibling set, interface satisfaction, and
//!   `$deps` nested-instance provisioning (§13.4–§13.6). Binding an install to a
//!   live containing row is landed ([`ModuleHost::install`] rejects an install into
//!   a module collection whose containing row does not exist, §13.2/§13.3).
//! - Interface-addressed *mutation* dispatch and cross-module atomic transitions
//!   (§13.10/§13.11); `$if_module`-guarded declaration activation (§13.7).

pub(crate) mod address;
mod compat;
mod host;
mod install;
mod merge;
mod mounted;
mod parent;
mod peer;
mod recorder;
mod value;

pub use host::{DecodedPackageId, ModuleHost};
pub use install::{AdmittedBindings, DepSpec, InstallRequest, UseSpec};
pub use merge::SeedMerge;
pub(crate) use mounted::MountedModules;
// §13.16: the structured refusal an `update_module(… { migrate: model+data })`
// hands an external tool instead of merging a divergent history.
pub use value::AncestryDivergence;

use liasse_store::CommitSeq;

use crate::error::{EngineError, Rejection, RejectionReason};

/// The observable result of a successful §13.14 single-instance module update,
/// assembled into the §13.15 update report by the driver.
///
/// It carries the version movement and committed result plus the three grouped
/// boundary reports §13.15 pins: `$migrated`/`$seeded` per-item paths in canonical
/// path order, the `$exposed` interface grouping (`$unchanged`/`$changed`/
/// `$removed`), and the `$imports` grouping (`$rebound`/`$broken`). The `$instance`
/// display path and `$commit` rendering are the driver's to add — the runtime
/// reports the migration facts, not the addressing of the instance in the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleUpdateReport {
    /// The instance's version before the update (`$from`), `major.minor.patch`.
    pub from: String,
    /// The target version the update moved to (`$to`), `major.minor.patch`.
    pub to: String,
    /// The commit the accepted update took (`$commit`).
    pub commit: CommitSeq,
    /// The migrated rows' canonical display paths, in canonical path order
    /// (`$migrated`).
    pub migrated: Vec<String>,
    /// The seed rows inserted where absent, canonical display paths (`$seeded`).
    pub seeded: Vec<String>,
    /// Exposed interfaces whose boundary contract is unchanged (`$exposed.$unchanged`).
    pub exposed_unchanged: Vec<String>,
    /// Exposed interfaces whose boundary contract compatibly widened
    /// (`$exposed.$changed`).
    pub exposed_changed: Vec<String>,
    /// Exposed interfaces the update no longer exposes (`$exposed.$removed`).
    pub exposed_removed: Vec<String>,
    /// Import handles the update re-bound to their sources (`$imports.$rebound`).
    pub imports_rebound: Vec<String>,
    /// Import handles the update could no longer bind (`$imports.$broken`).
    pub imports_broken: Vec<String>,
}

/// A failure of a module lifecycle operation (§13.3).
#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    /// The instance name is empty (§13.3: "a non-empty text value").
    #[error("an instance name must be a non-empty text value")]
    EmptyName,
    /// The instance name already names a live instance in this module collection
    /// (§13.3: "unique within its module collection"). BAD INPUT, not an internal
    /// fault: the caller asked to install under a name that is taken, so every
    /// surface reports it as a rejection rather than an error.
    #[error("instance name `{0}` is already installed in this module collection")]
    DuplicateName(String),
    /// No instance is mounted at the addressed entry.
    #[error("no module instance is mounted at `{0}`")]
    Unknown(String),
    /// The addressed instance is disabled, so its boundary occurrences are
    /// unavailable (§13.3, §13.12).
    #[error("instance `{0}` is disabled")]
    Disabled(String),
    /// The module collection's containing row is not live in root state, so the
    /// collection does not exist and there is nothing to install into (§13.2/§13.3:
    /// an install creates an instance inside an existing module collection, which
    /// exists at the location of each containing row). Rejects a ghost-row install
    /// (e.g. into `/companies/ghost/modules` when no `ghost` company row exists).
    /// BAD INPUT, not an internal fault: the caller addressed a row that is not
    /// there, so every surface reports it as a rejection rather than an error.
    #[error("the module collection `{0}` has no containing row in root state")]
    MissingContainingRow(String),
    /// A `$use`/`$deps` binding spec is malformed (§13.5/§13.6).
    #[error("`{0}` is not a valid module binding spec")]
    InvalidBinding(String),
    /// A required peer `$use` handle could not be resolved against the sibling
    /// instance set at install (§13.5 resolution): zero compatible candidates in the
    /// same module collection, several compatible candidates with no explicit
    /// `$use` binding, an incompatible major, a candidate that is disabled (§13.12
    /// removes peer availability), or an explicit binding that names a non-sibling
    /// instance. An admission refusal, not a static invalidity: the
    /// package itself is well-formed but no binding satisfies the requirement here.
    #[error("peer binding `{0}` cannot be resolved: {1}")]
    PeerUnresolved(String, String),
    /// A child's `$expose` does not structurally satisfy the module collection's
    /// declared interface contract (§13.8): a required `$view` field is missing or mistyped,
    /// or the interface is not exposed at all. Rejected before the instance activates
    /// (§13.3 "Loading validates ... interfaces ... before the instance becomes
    /// active").
    #[error("the child does not satisfy interface contract `{0}`: {1}")]
    InterfaceContract(String, String),
    /// An installation `$config` value does not match the child's declared `$config`
    /// typed struct (§13.1), or names a field the struct does not declare.
    #[error("installation `$config` does not match the declared struct: {0}")]
    ConfigMismatch(String),
    /// A minor/patch update definitionally narrows the module's own exposed
    /// compatibility surface (§13.14): an exposed `$view` drops a field, or an
    /// exposed operation the module no longer provides (its backing private mutation
    /// is gone too). A purely definitional comparison of the old and new `$expose`,
    /// independent of composition state, so it is a static "package loading" refusal
    /// (`invalid`, tests/13-modules/NOTES.md) refused before the migration commits —
    /// the current release stays active (E.9).
    #[error("the update narrows the module's exposed compatibility surface: {0}")]
    ExposedNarrowed(String),
    /// A minor/patch update withdraws a previously accepted module-interface binding
    /// while the private implementation that satisfied it remains (§13.14, E.4). The
    /// candidate module is well-formed on its own; the defect is the removed boundary
    /// binding, caught by the §13.14 recheck of module exposures before admission (an
    /// admission `rejected`, tests/13-modules/NOTES.md) — the current binding stays
    /// active (E.9).
    #[error("the update withdraws a previously accepted interface binding: {0}")]
    InterfaceBindingWithdrawn(String),
    /// Loading or operating the child instance failed.
    #[error(transparent)]
    Engine(#[from] EngineError),
}

impl ModuleError {
    /// Reclassify this refusal as a [`Rejection`] when it is BAD INPUT rather than
    /// an internal fault, keeping the error for the cases that genuinely are one.
    ///
    /// The rule is the boundary between the two vocabularies: an ERROR is something
    /// the engine got wrong, a REJECTION is something the caller asked for that it
    /// may not have. A name already taken, a module collection whose containing row
    /// is not live, an instance that is absent or disabled, a malformed binding
    /// spec, an unresolvable peer, an unsatisfied interface contract and a
    /// mismatched `$config` are all the caller's input; only an engine/store fault
    /// and the two §13.14 update-compatibility refusals (which are classified by
    /// their own path) stay errors here.
    pub(crate) fn into_rejection(self) -> Result<Rejection, Self> {
        let reason = match &self {
            Self::EmptyName
            | Self::DuplicateName(_)
            | Self::Unknown(_)
            | Self::Disabled(_)
            | Self::MissingContainingRow(_)
            | Self::InvalidBinding(_)
            | Self::PeerUnresolved(..) => RejectionReason::Malformed,
            Self::InterfaceContract(..) | Self::ConfigMismatch(_) => RejectionReason::TypeError,
            Self::ExposedNarrowed(_) | Self::InterfaceBindingWithdrawn(_) | Self::Engine(_) => {
                return Err(self);
            }
        };
        let message = self.to_string();
        Ok(Rejection::new(reason, message))
    }
}
