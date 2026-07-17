//! Module composition runtime (§13).
//!
//! A [`ModuleHost`] owns a root [`Engine`](crate::Engine) and the child instances
//! installed in its **row-scoped module spaces** ([`ModuleSpace`], e.g.
//! `/companies/acme/modules`). Each installed instance is its own independently
//! loaded [`Engine`](crate::Engine) over a store the host's
//! [`StoreFactory`](liasse_store::StoreFactory) mints (§13.1: "each installed
//! instance owns its private model, data, history, configuration"), so isolation
//! is structural — nothing but the declared boundary crosses between instances.
//!
//! # What this layer does (CORE this increment)
//!
//! - **Install + mount** ([`ModuleHost::install`]): admits an [`InstallRequest`]
//!   into a [`ModuleSpace`], recording its `$config`/`$use`/`$deps` boundary
//!   bindings ([`AdmittedBindings`], §13.3 `$resolved`) on the new instance; the
//!   same package installed under two names or in two spaces yields isolated
//!   instances (§13.2).
//! - **Interface-addressed read** ([`ModuleHost::interface_read`],
//!   [`ModuleHost::aggregate`]): evaluates a child's `$expose`d interface `$view`
//!   through the boundary (§13.8) — only projected fields cross, so a private field
//!   is unreachable (isolation) — and aggregates one interface across every enabled
//!   instance in a space with inherited identity (§13.9, [`InterfaceRow`]).
//! - **Lifecycle** ([`ModuleHost::disable`]/[`ModuleHost::enable`]/
//!   [`ModuleHost::uninstall`], plus [`ModuleHost::rename`]/[`ModuleHost::update`]):
//!   disable removes a child's active boundary occurrences (so aggregation skips it)
//!   while retaining its private state and history; enable restores them (§13.3,
//!   §13.12).
//! - **Seed three-way merge** ([`SeedMerge`], §13.13) as a pure rule.
//!
//! # Documented seams (next rounds)
//!
//! - Installation `$data` overlay onto the child genesis (§13.3), and wiring the
//!   [`SeedMerge`] rule into the update seed pass (§13.13).
//! - Peer/parent resolution against the sibling set, interface satisfaction, and
//!   `$deps` nested-instance provisioning (§13.4–§13.6); binding a space to a
//!   declared `$modules` mount and checking the containing row exists (§13.2).
//! - Interface-addressed *mutation* dispatch and cross-module atomic transitions
//!   (§13.10/§13.11); `$if_module`-guarded declaration activation (§13.7).

mod aggregate;
mod host;
mod install;
mod merge;
mod space;

pub(crate) use aggregate::{AggregatedInstance, ModuleAggregate};
pub use host::ModuleHost;
pub use install::{AdmittedBindings, DepSpec, InstallRequest, UseSpec};
pub use merge::SeedMerge;
pub use space::ModuleSpace;

use crate::error::EngineError;
use crate::view::ViewRow;

/// A failure of a module lifecycle operation (§13.3).
#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    /// The instance name is empty (§13.3: "a non-empty text value").
    #[error("an instance name must be a non-empty text value")]
    EmptyName,
    /// The instance name already names a live instance in this space (§13.3:
    /// "unique within its module space").
    #[error("instance name `{0}` is already installed in this space")]
    DuplicateName(String),
    /// No instance of that name is installed in the addressed space.
    #[error("no installed instance named `{0}`")]
    Unknown(String),
    /// The addressed instance is disabled, so its boundary occurrences are
    /// unavailable (§13.3, §13.12).
    #[error("instance `{0}` is disabled")]
    Disabled(String),
    /// The mount path is not a well-formed module-space location (§13.2).
    #[error("`{0}` is not an absolute module-space mount path")]
    InvalidSpace(String),
    /// A `$use`/`$deps` binding spec is malformed (§13.5/§13.6).
    #[error("`{0}` is not a valid module binding spec")]
    InvalidBinding(String),
    /// A child's `$expose` does not structurally satisfy the module space's declared
    /// interface contract (§13.8): a required `$view` field is missing or mistyped,
    /// or the interface is not exposed at all. Rejected before the instance activates
    /// (§13.3 "Loading validates ... interfaces ... before the instance becomes
    /// active").
    #[error("the child does not satisfy interface contract `{0}`: {1}")]
    InterfaceContract(String, String),
    /// An installation `$config` value does not match the child's declared `$config`
    /// typed struct (§13.1), or names a field the struct does not declare.
    #[error("installation `$config` does not match the declared struct: {0}")]
    ConfigMismatch(String),
    /// Loading or operating the child instance failed.
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// One row of a §13.9 interface aggregation: the instance it came from and the
/// exposed row read through that instance's interface. Its inherited identity is
/// the module instance identity plus the exposed row identity (§13.9): the
/// [`instance`](Self::instance) name keys the source instance and
/// [`row`](Self::row) carries only the boundary-projected fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceRow {
    instance: String,
    row: ViewRow,
}

impl InterfaceRow {
    /// The source instance name within the space (§13.9 module instance identity).
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// The exposed row read through the boundary — only the fields the interface
    /// `$view` projects (§13.8 isolation).
    #[must_use]
    pub fn row(&self) -> &ViewRow {
        &self.row
    }
}
