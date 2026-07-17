//! The module composition host: a root engine plus the child instances mounted in
//! its row-scoped module spaces (§13).

use liasse_ident::InstanceId;
use liasse_store::StoreFactory;

use crate::engine::Engine;
use crate::error::EngineError;
use crate::generator::Generators;
use crate::modules::install::{AdmittedBindings, InstallRequest};
use crate::modules::{InterfaceRow, ModuleError, ModuleSpace};
use crate::outcome::CallOutcome;
use crate::request::CallRequest;
use crate::view::ViewResult;

/// One installed child module instance mounted in a space.
struct Child<S> {
    /// The module space this instance is mounted in (§13.2).
    space: ModuleSpace,
    /// The instance name, the local component of its identity within the space
    /// (§13.3). Mutable: a rename is a rekey (§13.3).
    name: String,
    /// The immutable incarnation, preserved across rename (D.1).
    incarnation: InstanceId,
    /// The child's own loaded engine over its private store — a wholly separate
    /// instance, so isolation is structural (§13.1).
    engine: Engine<S>,
    /// The boundary bindings admitted at install (§13.3 `$resolved`).
    bindings: AdmittedBindings,
    /// Whether the child's active boundary occurrences are available (§13.3/§13.12
    /// disable/enable). A disabled child keeps its private state and history.
    enabled: bool,
}

impl<S> Child<S> {
    fn is(&self, space: &ModuleSpace, name: &str) -> bool {
        &self.space == space && self.name == name
    }
}

/// A root application together with the module instances installed in its
/// row-scoped module spaces (§13.2). Each child is an independently loaded
/// [`Engine`] over a store the host's [`StoreFactory`] mints, so two installs of
/// the same package — in the same space under different names, or in two spaces —
/// are isolated instances (§13.1/§13.2).
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

    /// Install a new instance into `space` from an install `request` (§13.3),
    /// admitting its `$config`/`$use`/`$deps` boundary bindings. Rejects an empty or
    /// duplicate name and a malformed binding; otherwise mints a fresh incarnation,
    /// creates the child's private store, loads its engine (applying its own `$data`
    /// seed), and records the admitted bindings on the instance.
    ///
    /// The installation `$data` overlay onto the child genesis (§13.3), peer
    /// resolution against the sibling set (§13.5), and reading `$config` through
    /// child expressions (§13.1) are documented seams; the bindings are recorded so
    /// a later pass can apply them.
    pub fn install<G: Generators>(
        &mut self,
        space: &ModuleSpace,
        request: InstallRequest,
        generator: &mut G,
    ) -> Result<InstanceId, ModuleError> {
        let (name, definition, bindings) = request.admit()?;
        if self.find(space, &name).is_some() {
            return Err(ModuleError::DuplicateName(name));
        }
        let incarnation = self.mint_incarnation(space, &name);
        let store = self
            .factory
            .create(incarnation.clone())
            .map_err(|error| ModuleError::Engine(EngineError::Store(error)))?;
        let engine = Engine::load(store, &definition, generator)?;
        self.children.push(Child {
            space: space.clone(),
            name,
            incarnation: incarnation.clone(),
            engine,
            bindings,
            enabled: true,
        });
        Ok(incarnation)
    }

    /// Whether an instance of that name is installed in `space` (enabled or
    /// disabled).
    #[must_use]
    pub fn is_installed(&self, space: &ModuleSpace, name: &str) -> bool {
        self.find(space, name).is_some()
    }

    /// Whether the named instance in `space` is installed and enabled.
    #[must_use]
    pub fn is_enabled(&self, space: &ModuleSpace, name: &str) -> bool {
        self.find(space, name).is_some_and(|child| child.enabled)
    }

    /// The incarnation of the named instance in `space`, if installed.
    #[must_use]
    pub fn incarnation(&self, space: &ModuleSpace, name: &str) -> Option<&InstanceId> {
        self.find(space, name).map(|child| &child.incarnation)
    }

    /// The admitted boundary bindings of the named instance in `space` (§13.3).
    #[must_use]
    pub fn bindings(&self, space: &ModuleSpace, name: &str) -> Option<&AdmittedBindings> {
        self.find(space, name).map(|child| &child.bindings)
    }

    /// Disable an instance (§13.3, §13.12): remove its active boundary occurrences,
    /// external surfaces, and peer availability while retaining its private stored
    /// state and history. The child engine and store are kept intact, so a later
    /// [`ModuleHost::enable`] restores the exact preserved state.
    pub fn disable(&mut self, space: &ModuleSpace, name: &str) -> Result<(), ModuleError> {
        self.child_mut(space, name)?.enabled = false;
        Ok(())
    }

    /// Enable a disabled instance (§13.3): revalidate and restore its boundary
    /// occurrences over the exact preserved private state.
    pub fn enable(&mut self, space: &ModuleSpace, name: &str) -> Result<(), ModuleError> {
        self.child_mut(space, name)?.enabled = true;
        Ok(())
    }

    /// Uninstall an instance (§13.3, §13.12): remove the instance incarnation and
    /// its owned subtree.
    pub fn uninstall(&mut self, space: &ModuleSpace, name: &str) -> Result<(), ModuleError> {
        match self.children.iter().position(|child| child.is(space, name)) {
            Some(index) => {
                self.children.remove(index);
                Ok(())
            }
            None => Err(ModuleError::Unknown(name.to_owned())),
        }
    }

    /// Rename an instance within its space (§13.3): a rekey that preserves the
    /// incarnation and therefore the durable identity (D.1). Rejects a name already
    /// in use in the same space.
    pub fn rename(&mut self, space: &ModuleSpace, from: &str, to: &str) -> Result<(), ModuleError> {
        if self.find(space, to).is_some() {
            return Err(ModuleError::DuplicateName(to.to_owned()));
        }
        self.child_mut(space, from)?.name = to.to_owned();
        Ok(())
    }

    /// Update a single instance to a target definition (§13.14): delegates to the
    /// §20 migration over the child's own engine, affecting that instance only.
    pub fn update<G: Generators>(
        &mut self,
        space: &ModuleSpace,
        name: &str,
        target: &str,
        generator: &mut G,
    ) -> Result<crate::migrate::UpdateReport, ModuleError> {
        let child = self.child_mut(space, name)?;
        child.engine.update(target, generator).map_err(|error| match error {
            crate::migrate::UpdateError::Engine(engine) => ModuleError::Engine(engine),
            other => ModuleError::Engine(EngineError::Internal(other.to_string())),
        })
    }

    /// Admit a mutation call against an enabled child instance (§13.11 direct
    /// module surface). A disabled instance has no active surfaces.
    pub fn child_call<G: Generators>(
        &mut self,
        space: &ModuleSpace,
        name: &str,
        request: &CallRequest,
        generator: &mut G,
    ) -> Result<CallOutcome, ModuleError> {
        let child = self.enabled_child_mut(space, name)?;
        child.engine.call(request, generator).map_err(ModuleError::Engine)
    }

    /// Read an enabled child instance's exposed interface `$view` through the
    /// boundary (§13.8): only the fields the exposed projection selects cross, so a
    /// private field is unreachable here (§13.8 isolation). A disabled instance
    /// exposes no boundary occurrences (§13.12). `None` when the child declares no
    /// readable interface of that name.
    pub fn interface_read(
        &self,
        space: &ModuleSpace,
        name: &str,
        interface: &str,
    ) -> Result<Option<ViewResult>, ModuleError> {
        let child = self.enabled_child(space, name)?;
        child.engine.interface_read(interface).map_err(ModuleError::Engine)
    }

    /// Aggregate one exposed interface across every enabled instance in `space`
    /// (§13.9 "The parent reads every instance exposing an interface"). Each row
    /// carries its inherited identity — the instance name plus the exposed row
    /// (§13.9). A disabled instance is skipped, so disabling removes it from the
    /// aggregation (§13.12); instances are visited in installation order.
    pub fn aggregate(
        &self,
        space: &ModuleSpace,
        interface: &str,
    ) -> Result<Vec<InterfaceRow>, ModuleError> {
        let mut rows = Vec::new();
        for child in self.children.iter().filter(|c| &c.space == space && c.enabled) {
            let Some(result) = child.engine.interface_read(interface).map_err(ModuleError::Engine)? else {
                continue;
            };
            for row in result.rows() {
                rows.push(InterfaceRow { instance: child.name.clone(), row: row.clone() });
            }
        }
        Ok(rows)
    }

    /// Evaluate a named child view at head — the §13.11 *direct* module surface a
    /// host mounts, distinct from the [`ModuleHost::interface_read`] boundary read.
    /// Only an enabled instance exposes its surfaces (§13.12).
    pub fn child_view(&self, space: &ModuleSpace, name: &str, view: &str) -> Result<Option<ViewResult>, ModuleError> {
        let child = self.enabled_child(space, name)?;
        child.engine.view_at_head(view).map_err(ModuleError::Engine)
    }

    fn find(&self, space: &ModuleSpace, name: &str) -> Option<&Child<F::Store>> {
        self.children.iter().find(|child| child.is(space, name))
    }

    fn child_mut(&mut self, space: &ModuleSpace, name: &str) -> Result<&mut Child<F::Store>, ModuleError> {
        self.children
            .iter_mut()
            .find(|child| child.is(space, name))
            .ok_or_else(|| ModuleError::Unknown(name.to_owned()))
    }

    fn enabled_child(&self, space: &ModuleSpace, name: &str) -> Result<&Child<F::Store>, ModuleError> {
        let child = self.find(space, name).ok_or_else(|| ModuleError::Unknown(name.to_owned()))?;
        if child.enabled {
            Ok(child)
        } else {
            Err(ModuleError::Disabled(name.to_owned()))
        }
    }

    fn enabled_child_mut(&mut self, space: &ModuleSpace, name: &str) -> Result<&mut Child<F::Store>, ModuleError> {
        let child = self.child_mut(space, name)?;
        if child.enabled {
            Ok(child)
        } else {
            Err(ModuleError::Disabled(name.to_owned()))
        }
    }

    fn mint_incarnation(&mut self, space: &ModuleSpace, name: &str) -> InstanceId {
        let token = format!(
            "{}#m{}-{}-{name}",
            self.root.instance().as_str(),
            self.next_incarnation,
            space.as_str().trim_start_matches('/').replace('/', "."),
        );
        self.next_incarnation += 1;
        InstanceId::new(token)
    }
}
