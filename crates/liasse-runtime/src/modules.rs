//! Module composition runtime (§13).
//!
//! A [`ModuleHost`] owns a root [`Engine`] and the installed child instances
//! mounted in its module spaces, each its own independently loaded [`Engine`]
//! over a store the host's [`StoreFactory`] mints (§13.1: "each installed instance
//! owns its private model, data, history, configuration"). The lifecycle
//! operations of §13.3/§13.12 are engine operations over those child instances:
//!
//! - [`ModuleHost::install`] loads a new named child instance (§13.3);
//! - [`ModuleHost::disable`]/[`ModuleHost::enable`] remove and restore a child's
//!   active surfaces while **retaining its private stored state and history**
//!   (§13.3, §13.12) — a disabled child's engine and store are kept intact;
//! - [`ModuleHost::uninstall`] removes the instance and its owned subtree;
//! - [`ModuleHost::rename`] is a rekey that preserves the child incarnation
//!   (§13.3, D.1: renaming preserves identity).
//!
//! CORE scope: isolation between instances (each is a wholly separate engine and
//! store), the disable/enable state-preservation invariant, single-instance
//! update (§13.14, delegating to §20 migration), and the §13.13 seed three-way
//! merge as a pure rule. Cross-instance boundary bindings (`$use`/`$expose`/
//! `$deps`), interface satisfaction, and cross-module atomic transactions need
//! the interface-resolution runtime and remain documented seams; a child is
//! addressed here directly by its instance name.

use liasse_ident::InstanceId;
use liasse_store::StoreFactory;
use liasse_value::Value;

use crate::engine::Engine;
use crate::error::EngineError;
use crate::generator::Generators;
use crate::materialize::FieldMap;
use crate::outcome::CallOutcome;
use crate::request::CallRequest;
use crate::view::ViewResult;

/// A failure of a module lifecycle operation (§13.3).
#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    /// The instance name already names a live instance in this space (§13.3:
    /// "unique within its module space").
    #[error("instance name `{0}` is already installed")]
    DuplicateName(String),
    /// No instance of that name is installed.
    #[error("no installed instance named `{0}`")]
    Unknown(String),
    /// The addressed instance is disabled, so its surfaces are unavailable
    /// (§13.3, §13.12).
    #[error("instance `{0}` is disabled")]
    Disabled(String),
    /// Loading or operating the child instance failed.
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// One installed child module instance.
struct Child<S> {
    /// The instance name, the local component of its identity (§13.3). Mutable:
    /// a rename is a rekey (§13.3).
    name: String,
    /// The immutable incarnation, preserved across rename (D.1).
    incarnation: InstanceId,
    /// The child's own loaded engine over its private store.
    engine: Engine<S>,
    /// Whether the child's active surfaces are available (§13.3 disable/enable).
    enabled: bool,
}

/// A root application together with its installed module instances.
pub struct ModuleHost<F: StoreFactory> {
    factory: F,
    root: Engine<F::Store>,
    children: Vec<Child<F::Store>>,
    next_incarnation: u64,
}

impl<F: StoreFactory> ModuleHost<F> {
    /// Wrap a loaded root engine, ready to install module instances.
    pub fn new(factory: F, root: Engine<F::Store>) -> Self {
        Self { factory, root, children: Vec::new(), next_incarnation: 0 }
    }

    /// The root application engine.
    #[must_use]
    pub fn root(&self) -> &Engine<F::Store> {
        &self.root
    }

    /// The root application engine, mutably (to admit root requests).
    pub fn root_mut(&mut self) -> &mut Engine<F::Store> {
        &mut self.root
    }

    /// Install a new named child instance from a module `definition` (§13.3).
    /// Rejects a duplicate name; otherwise mints a fresh incarnation, creates the
    /// child's private store, and loads its engine (applying its own `$data` seed).
    pub fn install<G: Generators>(
        &mut self,
        name: &str,
        definition: &str,
        generator: &mut G,
    ) -> Result<InstanceId, ModuleError> {
        if self.find(name).is_some() {
            return Err(ModuleError::DuplicateName(name.to_owned()));
        }
        let incarnation = self.mint_incarnation(name);
        let store = self
            .factory
            .create(incarnation.clone())
            .map_err(|error| ModuleError::Engine(EngineError::Store(error)))?;
        let engine = Engine::load(store, definition, generator)?;
        self.children.push(Child {
            name: name.to_owned(),
            incarnation: incarnation.clone(),
            engine,
            enabled: true,
        });
        Ok(incarnation)
    }

    /// Whether an instance of that name is installed (enabled or disabled).
    #[must_use]
    pub fn is_installed(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    /// Whether the named instance is installed and enabled.
    #[must_use]
    pub fn is_enabled(&self, name: &str) -> bool {
        self.find(name).is_some_and(|child| child.enabled)
    }

    /// The incarnation of the named instance, if installed.
    #[must_use]
    pub fn incarnation(&self, name: &str) -> Option<&InstanceId> {
        self.find(name).map(|child| &child.incarnation)
    }

    /// Disable an instance (§13.3, §13.12): remove its active surfaces while
    /// retaining its private stored state and history. The child engine and its
    /// store are kept intact, so a later [`ModuleHost::enable`] restores the exact
    /// preserved state.
    pub fn disable(&mut self, name: &str) -> Result<(), ModuleError> {
        self.child_mut(name)?.enabled = false;
        Ok(())
    }

    /// Enable a disabled instance (§13.3): revalidate and restore its surfaces.
    /// The preserved private state becomes readable again unchanged.
    pub fn enable(&mut self, name: &str) -> Result<(), ModuleError> {
        self.child_mut(name)?.enabled = true;
        Ok(())
    }

    /// Uninstall an instance (§13.3, §13.12): remove the instance incarnation and
    /// its owned subtree.
    pub fn uninstall(&mut self, name: &str) -> Result<(), ModuleError> {
        match self.children.iter().position(|child| child.name == name) {
            Some(index) => {
                self.children.remove(index);
                Ok(())
            }
            None => Err(ModuleError::Unknown(name.to_owned())),
        }
    }

    /// Rename an instance (§13.3): a rekey that preserves the incarnation and
    /// therefore the durable identity (D.1). Rejects a name already in use.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), ModuleError> {
        if self.find(to).is_some() {
            return Err(ModuleError::DuplicateName(to.to_owned()));
        }
        self.child_mut(from)?.name = to.to_owned();
        Ok(())
    }

    /// Update a single instance to a target definition (§13.14): delegates to the
    /// §20 migration over the child's own engine, affecting that instance only.
    pub fn update<G: Generators>(
        &mut self,
        name: &str,
        target: &str,
        generator: &mut G,
    ) -> Result<crate::migrate::UpdateReport, ModuleError> {
        let child = self.child_mut(name)?;
        child
            .engine
            .update(target, generator)
            .map_err(|error| match error {
                crate::migrate::UpdateError::Engine(engine) => ModuleError::Engine(engine),
                other => ModuleError::Engine(EngineError::Internal(other.to_string())),
            })
    }

    /// Admit a mutation call against an enabled child instance (§13.11 direct
    /// module surface). A disabled instance has no active surfaces.
    pub fn child_call<G: Generators>(
        &mut self,
        name: &str,
        request: &CallRequest,
        generator: &mut G,
    ) -> Result<CallOutcome, ModuleError> {
        let child = self.enabled_child_mut(name)?;
        child.engine.call(request, generator).map_err(ModuleError::Engine)
    }

    /// Evaluate a child instance's view at its head (§13.9 aggregation source).
    /// Only an enabled instance exposes its surfaces (§13.12); a disabled one
    /// still holds the state but is skipped by aggregation.
    pub fn child_view(&self, name: &str, view: &str) -> Result<Option<ViewResult>, ModuleError> {
        let child = self.enabled_child(name)?;
        child.engine.view_at_head(view).map_err(ModuleError::Engine)
    }

    fn find(&self, name: &str) -> Option<&Child<F::Store>> {
        self.children.iter().find(|child| child.name == name)
    }

    fn child_mut(&mut self, name: &str) -> Result<&mut Child<F::Store>, ModuleError> {
        self.children
            .iter_mut()
            .find(|child| child.name == name)
            .ok_or_else(|| ModuleError::Unknown(name.to_owned()))
    }

    fn enabled_child(&self, name: &str) -> Result<&Child<F::Store>, ModuleError> {
        let child = self.find(name).ok_or_else(|| ModuleError::Unknown(name.to_owned()))?;
        if child.enabled {
            Ok(child)
        } else {
            Err(ModuleError::Disabled(name.to_owned()))
        }
    }

    fn enabled_child_mut(&mut self, name: &str) -> Result<&mut Child<F::Store>, ModuleError> {
        let child = self.child_mut(name)?;
        if child.enabled {
            Ok(child)
        } else {
            Err(ModuleError::Disabled(name.to_owned()))
        }
    }

    fn mint_incarnation(&mut self, name: &str) -> InstanceId {
        let token = format!("{}#m{}-{name}", self.root.instance().as_str(), self.next_incarnation);
        self.next_incarnation += 1;
        InstanceId::new(token)
    }
}

/// The §13.13 three-way seed merge over one row's fields: for each seeded field,
/// the new seed replaces the value only when the current value still equals the
/// old seed value; otherwise the current value is retained as local data.
pub struct SeedMerge<'a> {
    /// The old package seed values.
    pub old_seed: &'a FieldMap,
    /// The new package seed values.
    pub new_seed: &'a FieldMap,
    /// The current instance state.
    pub current: &'a FieldMap,
}

impl SeedMerge<'_> {
    /// Compute the merged field map (§13.13).
    #[must_use]
    pub fn merge(&self) -> FieldMap {
        let mut merged = FieldMap::new();
        let mut names: Vec<&String> = self
            .old_seed
            .keys()
            .chain(self.new_seed.keys())
            .chain(self.current.keys())
            .collect();
        names.sort();
        names.dedup();
        for name in names {
            if let Some(value) = Self::merge_field(
                self.old_seed.get(name),
                self.new_seed.get(name),
                self.current.get(name),
            ) {
                merged.insert(name.clone(), value);
            }
        }
        merged
    }

    /// Merge one field: replace with the new seed only when the current value is
    /// still the old seed value; otherwise keep the current (local) value.
    fn merge_field(old: Option<&Value>, new: Option<&Value>, current: Option<&Value>) -> Option<Value> {
        if current == old {
            new.cloned()
        } else {
            current.cloned()
        }
    }
}
