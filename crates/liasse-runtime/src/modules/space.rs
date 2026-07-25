//! The mount point of a module space (§13.2).

use liasse_ident::{CanonicalPath, KeyText, NameSegment, PathSegment};
use liasse_store::RowAddress;

use crate::modules::ModuleError;

/// The row-scoped location a set of independently configured module instances is
/// installed under (§13.2), e.g. `/companies/acme/modules`. The same package
/// installed in two spaces (`/companies/acme/modules`, `/companies/globex/modules`)
/// yields two independent instances (§13.2 "Installing the same package in each
/// space creates two independent instances").
///
/// A space is the containing-row identity together with the module-space
/// declaration path; with the instance name it forms the local part of instance
/// identity (§13.3: "the containing row identity, module-space declaration path,
/// and instance name"). This type carries the canonical mount path and derives both
/// the [`declaration_path`](Self::declaration_path) (keyed against the root
/// package's compiled `$modules` declarations) and the
/// [`containing_row_steps`](Self::containing_row_steps) an install checks against
/// live root state (§13.2); the host consults a root-model accessor
/// (`Engine::contains_row`) to reject a ghost-row install into a declared space.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleSpace {
    path: String,
    components: Vec<String>,
}

impl ModuleSpace {
    /// Parse an absolute mount path into a module space. A module-space location is
    /// rooted at `/` and names at least one component (`/companies/acme/modules`);
    /// a relative, empty, or trailing-slash path is not a module-space location and
    /// is rejected as [`ModuleError::InvalidSpace`].
    pub fn new(path: impl Into<String>) -> Result<Self, ModuleError> {
        let path = path.into();
        let Some(body) = path.strip_prefix('/') else {
            return Err(ModuleError::InvalidSpace(path));
        };
        let components: Vec<String> = body.split('/').map(str::to_owned).collect();
        if components.iter().any(String::is_empty) {
            return Err(ModuleError::InvalidSpace(path));
        }
        Ok(Self { path, components })
    }

    /// The space a `$modules` node declared as `declaration` mounts at, under the
    /// row `containing` addresses — `None` for a top-level space, whose container is
    /// the package root (§13.2 "each containing row"). This is the constructor the
    /// §13.16 `<-` install lowering uses to turn a written `.modules[@id]` into the
    /// mount the host addresses, so it is where a wrong space would be *minted*.
    ///
    /// It cannot mint one silently, because it never builds the path by string
    /// concatenation: every segment goes through [`CanonicalPath`], the D.3 display
    /// path type, which **escapes each segment before joining**. A key holding a `/`
    /// or a `:` therefore encodes as `%2F`/`%3A` and cannot forge an extra path
    /// level — `/companies/{acme/x}/modules` renders `/companies/acme%2Fx/modules`,
    /// whose [`declaration_path`](Self::declaration_path) is still
    /// `["companies", "modules"]` rather than the `["companies", "x"]` a naive join
    /// would have produced. The same escaping is what
    /// [`containing_row_steps`](Self::containing_row_steps) hands
    /// `Engine::contains_row`, which compares against the row's own D.2 key text, so
    /// the round trip is exact.
    ///
    /// A key value with no D.2 key text (an empty component; a non-key-eligible
    /// value) yields [`ModuleError::InvalidSpace`] naming the address, rather than a
    /// path with a guessed segment.
    pub(crate) fn mounted_at(
        containing: Option<&RowAddress>,
        declaration: &str,
    ) -> Result<Self, ModuleError> {
        let mut segments = Vec::new();
        for step in containing.into_iter().flat_map(RowAddress::steps) {
            let components: Vec<liasse_value::Value> = step.key().components().cloned().collect();
            let key = KeyText::from_key_values(&components).map_err(|error| {
                ModuleError::InvalidSpace(format!(
                    "the row `{}` has no D.2 key text at `{}`: {error}",
                    containing.map_or_else(String::new, RowAddress::render),
                    step.name().as_str()
                ))
            })?;
            segments.push(PathSegment::Name(step.name().clone()));
            segments.push(PathSegment::Key(key));
        }
        segments.push(PathSegment::Name(NameSegment::new(declaration)));
        Self::new(CanonicalPath::new(segments).to_display_string())
    }

    /// The canonical absolute mount path (`/companies/acme/modules`).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.path
    }

    /// The path components in order (`["companies", "acme", "modules"]`).
    #[must_use]
    pub fn components(&self) -> &[String] {
        &self.components
    }

    /// The declaration-name path of the `$modules` node this space mounts, dropping
    /// the intervening row keys (§13.2/§13.3): `/companies/acme/modules` →
    /// `["companies", "modules"]`, `/modules` → `["modules"]`. A display path
    /// interleaves collection segments with the row key selecting each level, so the
    /// declaration path is the even-indexed components (collection, …, member). This
    /// keys the space against the root package's compiled `$modules` declarations.
    #[must_use]
    pub fn declaration_path(&self) -> Vec<String> {
        self.components.iter().step_by(2).cloned().collect()
    }

    /// The `(collection declaration name, key display text)` steps of this space's
    /// **containing row** (§13.2/§13.3: "the containing row identity"): the space
    /// path minus its trailing `$modules` declaration name, read as the alternating
    /// collection/key pairs that address the row the `$modules` node hangs off.
    /// `/companies/acme/modules` → `[("companies", "acme")]`; a nested
    /// `/companies/acme/divisions/eu/modules` →
    /// `[("companies", "acme"), ("divisions", "eu")]`. A top-level `$modules` space
    /// (`/modules`) has **no** containing row — its container is the package root,
    /// which is always live — so the residual path is empty and this yields
    /// `Some(vec![])`. `None` when the residual path is not a well-formed sequence of
    /// collection/key pairs (an odd component count, so no single row is addressed);
    /// such a space names no containing row and cannot resolve one.
    #[must_use]
    pub fn containing_row_steps(&self) -> Option<Vec<(String, String)>> {
        let (_declaration, containing) = self.components.split_last()?;
        if containing.len() % 2 != 0 {
            return None;
        }
        Some(
            containing
                .chunks_exact(2)
                .filter_map(|pair| match pair {
                    [collection, key] => Some((collection.clone(), key.clone())),
                    _ => None,
                })
                .collect(),
        )
    }
}
